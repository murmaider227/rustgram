//! TGB-2967: pages loaded for the history (older preload pages, forum topic pages) must not be
//! announced to the open chat as new messages, and must not touch its unread counter, the
//! notifications or the loaded history (`NewMessageType::Existing`,
//! `history/history.cpp:669-700`; `Data::RepliesList` stores topic pages the same way,
//! `data/data_replies_list.cpp:746`).

use tg_api::convert::message_from_tl;
use tg_data::{ChannelId, MsgId as DataMsgId, PeerId as DataPeerId};
use tg_tl::{
    Identifiable, Serializable, enums as tl_enums, functions as tl_fns, types as tl_types,
};

use super::tests_reply_targets::{PEER_ID, build_live, tl_message, wait_until};
use crate::LiveSession;
use tg_app_core::history::backend::{HistoryBackend, HistoryLiveUpdate};
use tg_app_core::stand_in::types::{MsgId, PeerId};

const GROUP_ID: i64 = 700;
const TOPIC_ROOT: i32 = 5;

fn group_data_peer() -> DataPeerId {
    DataPeerId::from_channel(ChannelId(GROUP_ID as u64))
}

fn group_peer() -> PeerId {
    PeerId(group_data_peer().0)
}

fn tl_group_peer() -> tl_enums::Peer {
    tl_enums::Peer::Channel(tl_types::PeerChannel {
        channel_id: GROUP_ID,
    })
}

fn page(ids: std::ops::Range<i32>) -> Vec<u8> {
    let messages: Vec<_> = ids
        .rev()
        .map(|id| tl_message(id, tl_group_peer(), PEER_ID, "older", None))
        .collect();
    tl_enums::messages::Messages::ChannelMessages(tl_types::messages::ChannelMessages {
        inexact: false,
        pts: 1,
        count: 1100,
        offset_id_offset: None,
        messages,
        topics: Vec::new(),
        chats: Vec::new(),
        users: Vec::new(),
    })
    .to_bytes()
}

/// The group's bottom page 1000..=1099 is loaded and open; 1051.. are unread.
fn open_group() -> (super::tests_reply_targets::FakeRpcClient, LiveSession) {
    let bottom: Vec<_> = (1000..1100)
        .map(|id| tl_message(id, tl_group_peer(), PEER_ID, "recent", None))
        .collect();
    let (rpc, mut live) = build_live(group_data_peer(), bottom);
    live.write_session(|s| {
        let h = s.ensure_history(group_data_peer());
        h.inbox_read(DataMsgId(1050), Some(49));
        h.unread_count = Some(49);
    });
    rpc.set_handler(|ctor, _| {
        (ctor == tl_fns::messages::GetHistory::CONSTRUCTOR_ID
            || ctor == tl_fns::messages::GetReplies::CONSTRUCTOR_ID)
            .then(|| page(900..1000))
    });
    let _ = live.load_slice_around(group_peer(), MsgId::SHOW_AT_END, 50, 0);
    let _ = live.drain_history_updates(group_peer());
    let _ = live.bridge_state().drain_notify_events();
    (rpc, live)
}

/// Inserts a live incoming message as a fence and returns every message announced as added
/// to the open chat up to and including it (change events are delivered in order).
fn added_until_fence(live: &mut LiveSession) -> Vec<i32> {
    let fence = tl_message(1100, tl_group_peer(), PEER_ID, "fence", None);
    live.write_session(|s| {
        let self_id = s.self_user_id();
        if let Some(m) = message_from_tl(&fence, self_id) {
            s.insert_new_unread_message(m);
        }
    });
    let mut added = Vec::new();
    assert!(wait_until(|| {
        for update in live.drain_history_updates(group_peer()) {
            if let HistoryLiveUpdate::MessageAdded(m) = update {
                added.push(m.id.0);
            }
        }
        added.contains(&1100)
    }));
    added
}

struct Side {
    unread: Option<i32>,
    ranges: tg_data::SparseIdsList,
}

fn side(live: &LiveSession) -> Side {
    live.read_session(|s| {
        let h = s.history(group_data_peer()).expect("history");
        Side {
            unread: h.unread_count,
            ranges: h.loaded_ranges.clone(),
        }
    })
}

#[test]
fn an_older_history_page_of_100_messages_announces_no_new_messages() {
    let (rpc, mut live) = open_group();
    let older = live.load_slice_before(group_peer(), MsgId(1000), 100);
    assert_eq!(rpc.calls::<tl_fns::messages::GetHistory>().len(), 1);
    assert_eq!(older.messages.len(), 100, "the page is in the slice");

    assert_eq!(added_until_fence(&mut live), vec![1100]);
    assert_eq!(
        side(&live).unread,
        Some(50),
        "only the fence counts as unread"
    );
    assert!(
        live.bridge_state()
            .drain_notify_events()
            .iter()
            .all(|e| !matches!(e, tg_app_core::notifications::backend::IncomingNotifyEvent::Notification(n)
                if n.msg_id.0 < 1100)),
        "no notification for a page message"
    );
}

#[test]
fn a_forum_topic_page_of_100_messages_announces_no_new_messages() {
    let (rpc, mut live) = open_group();
    let before = side(&live);
    let topic = live.load_topic_messages_live(group_peer(), MsgId(TOPIC_ROOT));
    assert_eq!(rpc.calls::<tl_fns::messages::GetReplies>().len(), 1);
    assert_eq!(topic.len(), 100, "the topic page is returned");

    assert_eq!(added_until_fence(&mut live), vec![1100]);
    let after = side(&live);
    assert_eq!(after.unread, before.unread.map(|n| n + 1), "only the fence");
    let mut expected_ranges = before.ranges.clone();
    expected_ranges.add_new(DataMsgId(1100));
    assert_eq!(
        after.ranges, expected_ranges,
        "the topic page is not chat history"
    );
    assert!(
        live.read_session(|s| s
            .message(tg_data::FullMsgId::new(group_data_peer(), DataMsgId(950)))
            .is_some()),
        "topic messages stay stored for lookups"
    );
}
