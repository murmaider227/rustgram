//! Waiting for a background text send in live tests (TGB-2931): `send_message` returns the
//! pending local message at once and the server's answer arrives as a history update.

use std::time::{Duration, Instant};

use tg_app::history::backend::{HistoryBackend, HistoryLiveUpdate, HistoryMessage};
use tg_app::stand_in::types::{MsgId, PeerId};

/// How a background send of `local` ended.
#[derive(Debug)]
pub enum SendEnd {
    /// The server message that took the local message's place.
    Sent(Box<HistoryMessage>),
    /// The send failed with this error (if one was reported).
    Failed(Option<String>),
}

/// Drains `peer`'s history updates until the send of local message `local` ends.
pub fn wait_send_end(backend: &mut dyn HistoryBackend, peer: PeerId, local: MsgId) -> SendEnd {
    assert!(local.is_local(), "{local:?} is not a pending local message");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        for update in backend.drain_history_updates(peer) {
            match update {
                HistoryLiveUpdate::LocalMessageSent { local_id, message } if local_id == local => {
                    return SendEnd::Sent(message);
                }
                HistoryLiveUpdate::LocalMessageFailed { local_id, error } if local_id == local => {
                    return SendEnd::Failed(error);
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the send of {local:?} never finished");
}

/// [`wait_send_end`] for a send that must succeed; returns the server message.
pub fn wait_sent(backend: &mut dyn HistoryBackend, peer: PeerId, local: MsgId) -> HistoryMessage {
    match wait_send_end(backend, peer, local) {
        SendEnd::Sent(message) => *message,
        SendEnd::Failed(error) => panic!("the send of {local:?} failed: {error:?}"),
    }
}
