//! Sends the requests a newly opened chat needs at the same time instead of one after another.
//!
//! tdesktop issues them asynchronously when a chat is shown (`HistoryWidget::showHistory` →
//! `requestFullPeer`, the pinned-messages slice of `Data::PinnedMessages`, and the history
//! request), so their round trips overlap. Rustgram's history view asks for them through
//! synchronous backend calls (bot info, then the pinned list), each waiting for its own reply.
//! When the window shows a chat (`AppSession::prepare_open_chat`) both requests start on worker
//! threads. The backend calls do not wait for them: while one runs they answer from the cache
//! (no bot description, no pinned bar yet, as tdesktop shows before `requestFullPeer` and the
//! pinned slice answer) and the finished request wakes the UI, which asks again. A call whose
//! request was never started still loads it itself.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Condvar, Mutex};

use tg_data::PeerId as DataPeerId;

use crate::session::LiveSession;

/// Upper bound for waiting on a request another thread started; the requests themselves time
/// out sooner (`LIVE_RPC_TIMEOUT`, `BOTS_RPC_TIMEOUT`, `SEARCH_RPC_TIMEOUT`).
#[cfg(test)]
const WAIT_LIMIT: std::time::Duration = std::time::Duration::from_secs(20);

/// One of the requests started together for an opened chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum OpenChatRequest {
    /// `users.getFullUser` for a bot chat (`LiveSession::ensure_bot_full_loaded`).
    BotInfo,
    /// `messages.search` with `inputMessagesFilterPinned` (the pinned bar).
    Pinned,
    /// `channels.getParticipants(channelParticipantsAdmins)` for a supergroup: the admin and
    /// owner badges by sender names (`ChatParticipants::requestAdmins`).
    GroupAdmins,
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

    #[cfg(test)]
    fn wait(&self) {
        let Ok(guard) = self.finished.lock() else {
            return;
        };
        let _ = self
            .signal
            .wait_timeout_while(guard, WAIT_LIMIT, |finished| !*finished);
    }

    fn is_finished(&self) -> bool {
        self.finished.lock().is_ok_and(|f| *f)
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

/// `true` when [`start`] sent `needed` for `peer` and it has not been collected yet: the caller
/// answers from the cache and does not send it again (while it runs the cache may still be
/// empty; the finished request wakes the UI). Collects it once it finished. `false` when the
/// caller has to load it itself.
pub(crate) fn started_in_background(
    live: &LiveSession,
    peer: DataPeerId,
    needed: OpenChatRequest,
) -> bool {
    match take_in_flight(live, peer, needed) {
        Some(done) => {
            if done.is_finished()
                && let Ok(mut c) = live.cache.lock()
            {
                c.open_chat.in_flight.remove(&(peer, needed));
            }
            true
        }
        None => false,
    }
}

/// Blocks until the request `needed` for `peer` started by [`start`] finished (tests only).
#[cfg(test)]
pub(crate) fn wait_for_test(live: &LiveSession, peer: DataPeerId, needed: OpenChatRequest) {
    if let Some(done) = take_in_flight(live, peer, needed) {
        done.wait();
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
    if crate::sender_badge::group_admins_needed(live, peer) {
        list.push(OpenChatRequest::GroupAdmins);
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
                    OpenChatRequest::GroupAdmins => {
                        crate::sender_badge::load_group_admins_now(&session, peer)
                    }
                }
                worker_done.finish();
                session.bridge_state().notify_ui();
            });
        if spawned.is_err() {
            if let Ok(mut c) = live.cache.lock() {
                c.open_chat.in_flight.remove(&(peer, req));
            }
            done.finish();
        }
    }
}
