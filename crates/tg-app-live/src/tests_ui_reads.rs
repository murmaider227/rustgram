//! The chat header and the chat list page do not wait while the session is being written
//! (TGB-2932): they answer with the previous read and ask for another frame.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tg_data::{PeerId as DataPeerId, UserId as DataUserId};

use super::tests_reply_targets::{
    PEER_ID, build_live, build_live_with, test_runtime_config, tl_message, tl_user_peer, wait_until,
};
use tg_app_core::dialogs::backend::{DialogListQuery, DialogsBackend};
use tg_app_core::history::backend::HistoryBackend;
use tg_app_core::stand_in::types::{FilterId, PeerId};

/// How long the simulated session write holds the lock.
const WRITE_HOLD: Duration = Duration::from_millis(600);

fn peer() -> PeerId {
    PeerId(DataPeerId::from_user(DataUserId(PEER_ID as u64)).0)
}

#[test]
fn reads_with_an_earlier_answer_do_not_wait_for_a_session_write() {
    // The runtime's periodic tick (every 500 ms, `handle_tick`) takes the session for the
    // update timers, so during the 600 ms write below it always queues and takes the lock right
    // after it: the last read would then find the lock busy for a reason this test is not
    // about. Here only the first tick comes, right after startup (an interval fires at once);
    // under load it can still land behind the write, which the last check allows.
    let cfg = test_runtime_config().with_tick_interval(Duration::from_secs(3600));
    let (_rpc, live) = build_live_with(
        DataPeerId(peer().0),
        vec![tl_message(
            10,
            tl_user_peer(PEER_ID),
            PEER_ID,
            "hello",
            None,
        )],
        cfg,
    );
    // The first chat list read starts the background chat list load
    // (`start_chats_list_request`, then the preloader). With both lists already loaded it has
    // nothing to write; wait until it is over, so the only session writer below is this test's.
    live.write_session(|s| {
        s.set_folder_loaded(0, true);
        s.set_folder_loaded(1, true);
    });
    let list = DialogListQuery::Filter(FilterId::ALL);
    let header = live.peer_info(peer());
    let page = live.dialog_slice(list, 0, 20);
    assert!(header.is_some());
    assert!(
        wait_until(|| live.chats_list_loaded()),
        "the background chat list load finished"
    );

    // The session actor applies a long update batch: it holds the write lock.
    let session = Arc::clone(live.runtime().session());
    let handle = live.runtime().tokio_handle().clone();
    let (locked_tx, locked_rx) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        handle.block_on(async move {
            let _guard = session.write().await;
            let _ = locked_tx.send(());
            std::thread::sleep(WRITE_HOLD);
        });
    });
    locked_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("writer took the lock");
    let version_before = live.chat_version(peer());

    let started = Instant::now();
    let header_during = live.peer_info(peer());
    let page_during = live.dialog_slice(list, 0, 20);
    let waited = started.elapsed();
    eprintln!("two UI reads during a {WRITE_HOLD:?} session write took {waited:?}");
    assert!(waited < WRITE_HOLD / 4, "the UI waited {waited:?}");
    assert_eq!(header_during, header, "the previous chat header is shown");
    assert_eq!(page_during, page, "the previous chat list page is shown");
    assert_eq!(live.ui_reads.served_last(), 2);
    assert!(
        live.chat_version(peer()) > version_before,
        "the view is asked to read again on the next frame"
    );

    writer.join().expect("writer thread");
    // Once the writes queued behind this one are over, a read is fresh. The runtime's first tick
    // (`tokio::time::interval` fires at once) can still be queued here under load: the read
    // right after the join then gets the previous answer once more (TGB-3200, the next
    // test holds such a write on purpose).
    assert!(
        wait_until(|| {
            let served = live.ui_reads.served_last();
            let _ = live.peer_info(peer());
            live.ui_reads.served_last() == served
        }),
        "a read after the write is answered from the session"
    );
}

/// Polls `fut` once: `Some` when it is ready, `None` when it waits (a lock waiter is queued).
async fn poll_once<F: std::future::Future + Unpin>(fut: &mut F) -> Option<F::Output> {
    std::future::poll_fn(|cx| match std::pin::Pin::new(&mut *fut).poll(cx) {
        std::task::Poll::Ready(out) => std::task::Poll::Ready(Some(out)),
        std::task::Poll::Pending => std::task::Poll::Ready(None),
    })
    .await
}

/// Takes the session write lock on its own thread when `queue` says so, and holds it until
/// `release` sends. Sends on `locked` once it holds the lock; `queued` reports whether the
/// first attempt had to wait.
struct HeldWrite {
    release: mpsc::Sender<()>,
    locked: mpsc::Receiver<()>,
    queued: mpsc::Receiver<bool>,
    thread: std::thread::JoinHandle<()>,
}

impl HeldWrite {
    fn start(live: &crate::LiveSession) -> Self {
        let session = Arc::clone(live.runtime().session());
        let handle = live.runtime().tokio_handle().clone();
        let (release, release_rx) = mpsc::channel::<()>();
        let (locked_tx, locked) = mpsc::channel();
        let (queued_tx, queued) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            handle.block_on(async move {
                let mut write = Box::pin(session.write());
                let first = poll_once(&mut write).await;
                let _ = queued_tx.send(first.is_none());
                let _guard = match first {
                    Some(guard) => guard,
                    None => write.await,
                };
                let _ = locked_tx.send(());
                let _ = release_rx.recv_timeout(Duration::from_secs(10));
            });
        });
        Self {
            release,
            locked,
            queued,
            thread,
        }
    }

    fn wait_locked(&self) {
        self.locked
            .recv_timeout(Duration::from_secs(5))
            .expect("the writer took the lock");
    }

    fn end(self) {
        let _ = self.release.send(());
        self.thread.join().expect("writer thread");
    }
}

/// A session write queued while reads were answered from the cache takes the lock the moment
/// the first write ends: a read then still gets the previous answer, and the read after that
/// write is fresh. This is what the runtime's first tick did to the test above under load (its
/// last read was served from the cache: 3 instead of 2).
#[test]
fn a_write_queued_behind_another_keeps_the_previous_answer_until_it_ends() {
    let cfg = test_runtime_config().with_tick_interval(Duration::from_secs(3600));
    let (_rpc, live) = build_live_with(
        DataPeerId(peer().0),
        vec![tl_message(
            10,
            tl_user_peer(PEER_ID),
            PEER_ID,
            "hello",
            None,
        )],
        cfg,
    );
    let header = live.peer_info(peer());
    assert!(header.is_some());

    // Each read below runs while one of these two writes holds the lock, so it is served from
    // the cache whatever else the runtime does meanwhile.
    let first = HeldWrite::start(&live);
    first.wait_locked();
    let second = HeldWrite::start(&live);
    assert_eq!(
        second.queued.recv_timeout(Duration::from_secs(5)),
        Ok(true),
        "the second write waits behind the first"
    );
    assert_eq!(live.peer_info(peer()), header);
    assert_eq!(live.ui_reads.served_last(), 1);

    first.end();
    second.wait_locked();
    assert_eq!(
        live.peer_info(peer()),
        header,
        "the previous chat header is shown while the queued write runs"
    );
    assert_eq!(live.ui_reads.served_last(), 2);

    second.end();
    // The runtime's first tick may still take the lock once; after it a read is fresh.
    assert!(
        wait_until(|| {
            let served = live.ui_reads.served_last();
            let _ = live.peer_info(peer());
            live.ui_reads.served_last() == served
        }),
        "with no write left the read is fresh"
    );
    assert_eq!(live.peer_info(peer()), header);
}

#[test]
fn a_first_read_still_waits_for_the_answer() {
    let (_rpc, live) = build_live(
        DataPeerId(peer().0),
        vec![tl_message(
            10,
            tl_user_peer(PEER_ID),
            PEER_ID,
            "hello",
            None,
        )],
    );
    let session = Arc::clone(live.runtime().session());
    let handle = live.runtime().tokio_handle().clone();
    let (locked_tx, locked_rx) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        handle.block_on(async move {
            let _guard = session.write().await;
            let _ = locked_tx.send(());
            std::thread::sleep(Duration::from_millis(200));
        });
    });
    locked_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("writer took the lock");
    // No earlier answer to show: the header is read once the write is over (as before).
    assert!(live.peer_info(peer()).is_some());
    assert_eq!(live.ui_reads.served_last(), 0);
    writer.join().expect("writer thread");
}
