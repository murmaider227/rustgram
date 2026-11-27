//! Lock order between the session `RwLock` and the per-session side states.
//!
//! The update listeners take the session write lock first and a side state's `Mutex` second.
//! UI-thread code must never wait for the session while it holds such a `Mutex`, or both sides
//! wait for each other forever.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use tg_data::{
    FullMsgId as DataFullMsgId, MessageFlags, MsgId as DataMsgId, PeerId as DataPeerId,
    UserId as DataUserId,
};
use tg_tl::{enums as tl_enums, types as tl_types};

use super::tests_reply_targets::{PEER_ID, build_live, tl_message, tl_user_peer};
use tg_app_core::history::backend::{HistoryBackend, UnreadThingsScope};
use tg_app_core::stand_in::types::{MsgId, PeerId};

fn empty_updates() -> tl_enums::Updates {
    tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_750_000_000,
        seq: 0,
    })
}

/// `mark_visible_reactions_read` used to hold the engagement state while it read each scope's
/// count from the session; an update arriving in between (the listener holds the session write
/// lock and waits for the engagement state) froze both threads.
#[test]
fn marking_reactions_read_while_updates_arrive_does_not_deadlock() {
    let data_peer = DataPeerId::from_user(DataUserId(PEER_ID as u64));
    let ui_peer = PeerId(data_peer.0);
    let (rpc, live) = build_live(
        data_peer,
        vec![tl_message(10, tl_user_peer(PEER_ID), PEER_ID, "hi", None)],
    );
    // Many cached scopes of the chat: the old code read the session once per scope while it held
    // the engagement state, which leaves a wide window for the listener.
    let scopes: Vec<UnreadThingsScope> = (1..=200)
        .map(|top| UnreadThingsScope {
            peer_id: ui_peer,
            top_msg_id: Some(MsgId(top)),
            saved_peer_id: None,
        })
        .chain(std::iter::once(UnreadThingsScope::history(ui_peer)))
        .collect();
    crate::engagement::seed_unread_reaction_scopes(&live, &scopes);

    let stop = Arc::new(AtomicBool::new(false));
    let pusher = {
        let stop = Arc::clone(&stop);
        let rpc = rpc.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                rpc.push_updates(empty_updates());
                std::thread::yield_now();
            }
        })
    };

    let (done_tx, done_rx) = mpsc::channel();
    let mut worker_live = live.clone();
    std::thread::spawn(move || {
        for _ in 0..40 {
            worker_live.write_session(|s| {
                let id = DataFullMsgId::new(data_peer, DataMsgId(10));
                if let Some(m) = s.message_mut(id) {
                    m.flags |= MessageFlags::HAS_UNREAD_REACTION;
                }
                let h = s.ensure_history(data_peer);
                h.unread_reactions_count = 1;
                h.unread_reactions.insert(DataMsgId(10));
            });
            let _ = worker_live.mark_visible_reactions_read(ui_peer, &[MsgId(10)]);
        }
        let _ = done_tx.send(());
    });

    let finished = done_rx.recv_timeout(Duration::from_secs(60)).is_ok();
    stop.store(true, Ordering::Relaxed);
    let _ = pusher.join();
    if !finished {
        // The runtime's workers are stuck: dropping the session would wait for them forever.
        std::mem::forget(live);
        panic!("mark_visible_reactions_read deadlocked with the engagement updates listener");
    }
}
