//! Sends the requests a newly opened chat needs at the same time instead of one after another.
//!
//! tdesktop issues them asynchronously when a chat is shown (`HistoryWidget::showHistory` →
//! `requestFullPeer`, the pinned-messages slice of `Data::PinnedMessages`, and the history
//! request), so their round trips overlap. Rustgram's history view asks for them through
//! synchronous backend calls (bot info, then the pinned list), each waiting for its own reply.
//! When the window shows a chat (`AppSession::prepare_open_chat`) both requests start on worker
//! threads; the backend calls then wait for the request they need instead of sending it again,
//! so the replies arrive after one round trip instead of two and the data each call returns is
//! unchanged.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use tg_data::PeerId as DataPeerId;

use crate::session::LiveSession;

/// Upper bound for waiting on a request another thread started; the requests themselves time
/// out sooner (`LIVE_RPC_TIMEOUT`, `BOTS_RPC_TIMEOUT`, `SEARCH_RPC_TIMEOUT`).
const WAIT_LIMIT: Duration = Duration::from_secs(20);

/// One of the requests started together for an opened chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum OpenChatRequest {
    /// `users.getFullUser` for a bot chat (`LiveSession::ensure_bot_full_loaded`).
    BotInfo,
    /// `messages.search` with `inputMessagesFilterPinned` (the pinned bar).
    Pinned,
}

#[derive(Default)]
struct Done {
    finished: Mutex<bool>,
    signal: Condvar,
}

impl Done {
    fn finish(&self) {
        if let Ok(mut f) = self.finished.lock() {
            *f = true;
        }
        self.signal.notify_all();
    }

    fn wait(&self) {
        let Ok(guard) = self.finished.lock() else {
            return;
        };
        let _ = self
            .signal
            .wait_timeout_while(guard, WAIT_LIMIT, |finished| !*finished);
    }
}

/// Per-session record of the chats whose open requests were started and the ones in flight.
#[derive(Default)]
pub(crate) struct OpenChatRequests {
    started: HashSet<DataPeerId>,
    in_flight: HashMap<(DataPeerId, OpenChatRequest), Arc<Done>>,
}

fn take_in_flight(live: &LiveSession, peer: DataPeerId, req: OpenChatRequest) -> Option<Arc<Done>> {
    live.cache
        .lock()
        .ok()
        .and_then(|c| c.open_chat.in_flight.get(&(peer, req)).cloned())
}

/// Waits for `needed` if [`start`] sent it for `peer` and it has not been collected yet.
/// Returns `true` when it was handled that way (its result is now cached), `false` when the
/// caller has to load it itself.
pub(crate) fn wait_in_flight(
    live: &LiveSession,
    peer: DataPeerId,
    needed: OpenChatRequest,
) -> bool {
    match take_in_flight(live, peer, needed) {
        Some(done) => {
            done.wait();
            if let Ok(mut c) = live.cache.lock() {
                c.open_chat.in_flight.remove(&(peer, needed));
            }
            true
        }
        None => false,
    }
}

/// Starts the bot-info and pinned-messages requests for an opened chat on worker threads, once
/// per chat (`AppSession::prepare_open_chat`). Requests whose result is already cached are skipped.
pub(crate) fn start(live: &LiveSession, peer: DataPeerId) {
    let first = live
        .cache
        .lock()
        .is_ok_and(|mut c| c.open_chat.started.insert(peer));
    if !first {
        return;
    }
    // Checked without the cache lock held: both read the session, which can wait for its lock.
    let mut list = Vec::new();
    if live.bot_full_info_needed(peer) {
        list.push(OpenChatRequest::BotInfo);
    }
    if !crate::search_and_info::pinned_loaded(live, peer) {
        list.push(OpenChatRequest::Pinned);
    }
    let requests = {
        let Ok(mut c) = live.cache.lock() else {
            return;
        };
        let mut out = Vec::new();
        for req in list {
            let done = Arc::new(Done::default());
            c.open_chat.in_flight.insert((peer, req), Arc::clone(&done));
            out.push((req, done));
        }
        out
    };
    for (req, done) in requests {
        let session = live.clone();
        let worker_done = Arc::clone(&done);
        let spawned = std::thread::Builder::new()
            .name("open-chat-request".to_owned())
            .spawn(move || {
                match req {
                    OpenChatRequest::BotInfo => session.load_bot_full_info_now(peer),
                    OpenChatRequest::Pinned => {
                        crate::search_and_info::load_pinned_now(&session, peer)
                    }
                }
                worker_done.finish();
            });
        if spawned.is_err() {
            if let Ok(mut c) = live.cache.lock() {
                c.open_chat.in_flight.remove(&(peer, req));
            }
            done.finish();
        }
    }
}
