//! UI calls that used to wait for a round trip (typing, read receipts, drafts) return at once and
//! the requests still go out, in order (TGB-2893).

use std::time::{Duration, Instant};

use tg_data::{PeerId as DataPeerId, UserId as DataUserId};
use tg_tl::types as tl_types;
use tg_tl::{Identifiable, Serializable, enums as tl_enums, functions as tl_fns};

use super::tests_reply_targets::{PEER_ID, build_live, tl_message, tl_user_peer, wait_until};
use crate::LiveSession;
use tg_app_core::history::backend::{HistoryBackend, TypingAction};
use tg_app_core::stand_in::types::{MsgId, PeerId};

/// Simulated network round trip of the fake server.
const ROUND_TRIP: Duration = Duration::from_millis(400);

fn peer() -> PeerId {
    PeerId(DataPeerId::from_user(DataUserId(PEER_ID as u64)).0)
}

/// The harness's megagroup: typing goes out there (a user chat skips it unless the user is
/// online, `SendProgressManager::skipRequest`).
fn group() -> PeerId {
    PeerId(DataPeerId::from_channel(tg_data::ChannelId(700)).0)
}

fn open_chat() -> (super::tests_reply_targets::FakeRpcClient, LiveSession) {
    let (rpc, live) = build_live(
        DataPeerId(peer().0),
        vec![tl_message(
            10,
            tl_user_peer(PEER_ID),
            PEER_ID,
            "hello",
            None,
        )],
    );
    let _ = live.load_slice_around(peer(), MsgId::SHOW_AT_END, 50, 0);
    (rpc, live)
}

#[test]
fn typing_and_read_receipts_do_not_wait_for_the_server() {
    let (rpc, mut live) = open_chat();
    rpc.set_handler(|ctor, _| {
        if ctor == tl_fns::messages::SetTyping::CONSTRUCTOR_ID {
            std::thread::sleep(ROUND_TRIP);
            return Some(true.to_bytes());
        }
        if ctor == tl_fns::messages::ReadHistory::CONSTRUCTOR_ID {
            std::thread::sleep(ROUND_TRIP);
            return Some(
                tl_enums::messages::AffectedMessages::AffectedMessages(
                    tl_types::messages::AffectedMessages {
                        pts: 2,
                        pts_count: 1,
                    },
                )
                .to_bytes(),
            );
        }
        None
    });
    let started = Instant::now();
    live.send_typing(group(), TypingAction::Typing);
    live.mark_history_read(peer(), MsgId(10));
    let ui_wait = started.elapsed();
    eprintln!("send_typing + mark_history_read returned after {ui_wait:?}");
    assert!(ui_wait < ROUND_TRIP / 2, "the UI waited {ui_wait:?}");
    assert!(live.flush_background_rpcs(Duration::from_secs(10)));
    assert_eq!(rpc.calls::<tl_fns::messages::SetTyping>().len(), 1);
    assert_eq!(rpc.calls::<tl_fns::messages::ReadHistory>().len(), 1);
}

#[test]
fn a_new_message_moves_its_chat_version_and_the_chat_list_version() {
    let (_rpc, live) = open_chat();
    let (chat0, list0) = (
        live.chat_version(peer()),
        tg_app_core::dialogs::backend::DialogsBackend::dialogs_version(&live),
    );
    let session = std::sync::Arc::clone(live.runtime().session());
    live.runtime().tokio_handle().block_on(async move {
        let mut s = session.write().await;
        let self_id = s.self_user_id();
        let msg = tg_api::convert::message_from_tl(
            &tl_message(12, tl_user_peer(PEER_ID), PEER_ID, "new", None),
            self_id,
        )
        .expect("message");
        s.insert_message(msg);
    });
    assert!(wait_until(|| live.chat_version(peer()) > chat0));
    assert!(tg_app_core::dialogs::backend::DialogsBackend::dialogs_version(&live) > list0);
}

/// Chat with messages 50..=52 loaded at the bottom; the server holds 40..=49 before them and
/// answers `messages.getHistory` after [`ROUND_TRIP`].
fn chat_with_older_history_on_the_server()
-> (super::tests_reply_targets::FakeRpcClient, LiveSession) {
    let (rpc, live) = build_live(
        DataPeerId(peer().0),
        (50..=52)
            .map(|id| tl_message(id, tl_user_peer(PEER_ID), PEER_ID, "recent", None))
            .collect(),
    );
    rpc.set_handler(|ctor, _| {
        (ctor == tl_fns::messages::GetHistory::CONSTRUCTOR_ID).then(|| {
            std::thread::sleep(ROUND_TRIP);
            super::tests_reply_targets::plain_messages(
                (40..50)
                    .rev()
                    .map(|id| tl_message(id, tl_user_peer(PEER_ID), PEER_ID, "older", None))
                    .collect(),
                Vec::new(),
            )
        })
    });
    (rpc, live)
}

#[test]
fn loading_older_messages_does_not_wait_and_delivers_them_once_loaded() {
    use tg_app_core::history::backend::Loadable;

    let (rpc, live) = chat_with_older_history_on_the_server();
    let version_before = live.chat_version(peer());
    let started = Instant::now();
    let first = live.try_load_slice_before(peer(), MsgId(50), 40);
    let ui_wait = started.elapsed();
    eprintln!("try_load_slice_before returned {first:?} after {ui_wait:?} (server {ROUND_TRIP:?})");
    assert!(first.is_loading());
    assert!(ui_wait < ROUND_TRIP / 2, "the UI waited {ui_wait:?}");
    // Asking again every frame while it loads sends no second request.
    for _ in 0..5 {
        assert!(
            live.try_load_slice_before(peer(), MsgId(50), 40)
                .is_loading()
        );
    }
    let mut ready = None;
    assert!(wait_until(|| {
        match live.try_load_slice_before(peer(), MsgId(50), 40) {
            Loadable::Ready(slice) if !slice.messages.is_empty() => {
                ready = Some(slice);
                true
            }
            _ => false,
        }
    }));
    let slice = ready.expect("older slice");
    let ids: Vec<i32> = slice.messages.iter().map(|m| m.id.0).collect();
    assert_eq!(ids, (40..50).collect::<Vec<_>>());
    assert!(
        live.chat_version(peer()) > version_before,
        "the chat version moved"
    );
    assert_eq!(
        rpc.calls::<tl_fns::messages::GetHistory>().len(),
        1,
        "one request"
    );
}

#[test]
fn the_blocking_slice_read_returns_the_same_messages() {
    let (_rpc, live) = chat_with_older_history_on_the_server();
    let started = Instant::now();
    let slice = live.load_slice_before(peer(), MsgId(50), 40);
    eprintln!(
        "blocking load_slice_before took {:?} (server {ROUND_TRIP:?})",
        started.elapsed()
    );
    let ids: Vec<i32> = slice.messages.iter().map(|m| m.id.0).collect();
    assert_eq!(ids, (40..50).collect::<Vec<_>>());
}

/// Item 14 (callers): a page read walks only the messages it returns instead of collecting the
/// whole loaded history first. Same result, measured on 5,000 loaded messages.
#[test]
fn page_reads_walk_only_the_page_of_a_long_history() {
    use crate::convert::{history_messages_iter, history_messages_ordered};

    let total = 5_000;
    let (_rpc, live) = build_live(
        DataPeerId(peer().0),
        (1..=total)
            .map(|id| tl_message(id, tl_user_peer(PEER_ID), PEER_ID, "m", None))
            .collect(),
    );
    let data_peer = DataPeerId(peer().0);
    let target = tg_data::MsgId(i64::from(total - 10));
    let rounds = 200;
    let (old, new, old_ids, new_ids) = live.read_session(|s| {
        let started = Instant::now();
        let mut old_ids = Vec::new();
        for _ in 0..rounds {
            let all = history_messages_ordered(s, data_peer);
            old_ids = all
                .iter()
                .rev()
                .filter(|m| m.id < target)
                .take(40)
                .map(|m| m.id)
                .collect::<Vec<_>>();
        }
        let old = started.elapsed();
        let started = Instant::now();
        let mut new_ids = Vec::new();
        for _ in 0..rounds {
            new_ids = history_messages_iter(s, data_peer)
                .rev()
                .filter(|m| m.id < target)
                .take(40)
                .map(|m| m.id)
                .collect::<Vec<_>>();
        }
        (old, started.elapsed(), old_ids, new_ids)
    });
    eprintln!(
        "older page near the bottom of {total} messages: collect-all {:?}/read, iterator {:?}/read",
        old / rounds,
        new / rounds
    );
    assert_eq!(old_ids, new_ids);
    assert_eq!(new_ids.len(), 40);
}

#[test]
fn the_pinned_bar_does_not_wait_for_the_open_chat_prefetch() {
    use crate::open_chat_prefetch::{OpenChatRequest, start, wait_for_test};

    let (rpc, live) = open_chat();
    rpc.set_handler(|ctor, _| {
        (ctor == tl_fns::messages::Search::CONSTRUCTOR_ID).then(|| {
            std::thread::sleep(ROUND_TRIP);
            super::tests_reply_targets::plain_messages(Vec::new(), Vec::new())
        })
    });
    let data_group = DataPeerId(group().0);
    start(&live, data_group);
    let started = Instant::now();
    let pinned = live.pinned_messages(group());
    let ui_wait = started.elapsed();
    eprintln!("pinned_messages during the prefetch returned after {ui_wait:?}");
    assert!(ui_wait < ROUND_TRIP / 2, "the UI waited {ui_wait:?}");
    assert!(pinned.is_empty(), "no pinned bar until the answer lands");
    wait_for_test(&live, data_group, OpenChatRequest::Pinned);
    let _ = live.pinned_messages(group());
    assert_eq!(
        rpc.calls::<tl_fns::messages::Search>().len(),
        1,
        "the pinned list is requested once, by the prefetch"
    );
}
