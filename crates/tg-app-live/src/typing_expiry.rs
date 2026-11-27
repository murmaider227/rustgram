//! "Is typing" expires like in tdesktop (TGB-3128).
//!
//! `updateUserTyping` / `updateChatUserTyping` / `updateChannelUserTyping` only say that someone
//! started an action; the sender renews it every few seconds while it lasts and sends no end
//! when the app closes or the network drops. tdesktop shows an action for
//! `kStatusShowClientside*` (6 s, 10 s for a game, `history_view_send_action.cpp:26-39`) after
//! the last update, clears it on `sendMessageCancelAction`, and clears the sender's actions when
//! a message from them arrives (`SendActionPainter::clear`). The session keeps the raw actions
//! (`Session::set_typing_action`); [`TypingExpiry`] remembers when each one ends, and
//! [`run_typing_expiry`] cancels it in the session when that time comes, so the chat list row
//! and the chat header stop showing it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use tg_data::{ChangeEvent, MessageUpdateFlags, MsgId, PeerId, Session, TypingAction};
use tokio::sync::{Notify, RwLock};
use tokio::time::Instant;

/// `kStatusShowClientsideTyping` and the other actions but the game.
pub(crate) const ACTION_SHOWN_FOR: Duration = Duration::from_secs(6);
/// `kStatusShowClientsidePlayGame`.
pub(crate) const GAME_SHOWN_FOR: Duration = Duration::from_secs(10);

/// Chat, actor, forum topic.
type TypingKey = (PeerId, PeerId, Option<MsgId>);

/// When each shown action ends.
#[derive(Debug)]
pub(crate) struct TypingExpiry {
    until: HashMap<TypingKey, Instant>,
    action_for: Duration,
    game_for: Duration,
}

impl Default for TypingExpiry {
    fn default() -> Self {
        Self::with_lifetimes(ACTION_SHOWN_FOR, GAME_SHOWN_FOR)
    }
}

impl TypingExpiry {
    /// Actions shown for `action_for` after their last update, a game for `game_for`.
    pub(crate) fn with_lifetimes(action_for: Duration, game_for: Duration) -> Self {
        Self {
            until: HashMap::new(),
            action_for,
            game_for,
        }
    }

    /// An action update arrived at `now`: shown until its lifetime passes, or gone on cancel.
    pub(crate) fn record(&mut self, key: TypingKey, action: TypingAction, now: Instant) {
        match action {
            TypingAction::Cancel => {
                self.until.remove(&key);
            }
            TypingAction::PlayGame => {
                self.until.insert(key, now + self.game_for);
            }
            _ => {
                self.until.insert(key, now + self.action_for);
            }
        }
    }

    /// The actions whose time ran out by `now`; they are forgotten.
    pub(crate) fn take_expired(&mut self, now: Instant) -> Vec<TypingKey> {
        let expired: Vec<TypingKey> = self
            .until
            .iter()
            .filter(|&(_, &until)| until <= now)
            .map(|(&key, _)| key)
            .collect();
        for key in &expired {
            self.until.remove(key);
        }
        expired
    }

    /// The actions of `from` in `peer` (every topic), forgotten: a message from them arrived.
    pub(crate) fn take_from(&mut self, peer: PeerId, from: PeerId) -> Vec<TypingKey> {
        let gone: Vec<TypingKey> = self
            .until
            .keys()
            .filter(|&&(p, f, _)| p == peer && f == from)
            .copied()
            .collect();
        for key in &gone {
            self.until.remove(key);
        }
        gone
    }

    /// `true` when an action in `peer` is shown.
    pub(crate) fn has_peer(&self, peer: PeerId) -> bool {
        self.until.keys().any(|&(p, _, _)| p == peer)
    }

    /// The earliest end of a shown action.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.until.values().min().copied()
    }
}

/// [`TypingExpiry`] shared by the bridge's change loop and [`run_typing_expiry`].
#[derive(Debug, Default)]
pub(crate) struct TypingExpiryState {
    expiry: Mutex<TypingExpiry>,
    changed: Notify,
}

impl TypingExpiryState {
    #[cfg(test)]
    fn with_lifetimes(action_for: Duration, game_for: Duration) -> Self {
        Self {
            expiry: Mutex::new(TypingExpiry::with_lifetimes(action_for, game_for)),
            changed: Notify::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, TypingExpiry> {
        self.expiry.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Feeds one session change: typing updates start or end an action, and a new message ends
    /// its sender's actions in that chat at once.
    pub(crate) async fn on_change(&self, session: &RwLock<Session>, event: &ChangeEvent) {
        match *event {
            ChangeEvent::UserTyping {
                peer_id,
                from_id,
                top_msg_id,
                action,
            } => {
                self.lock()
                    .record((peer_id, from_id, top_msg_id), action, Instant::now());
                self.changed.notify_one();
            }
            ChangeEvent::MessageUpdated { id, flags }
                if flags.contains(MessageUpdateFlags::NEW_ADDED)
                    && !flags.contains(MessageUpdateFlags::DESTROYED) =>
            {
                if !self.lock().has_peer(id.peer) {
                    return;
                }
                let from = session
                    .read()
                    .await
                    .message(id)
                    .and_then(|m| m.from_id)
                    .unwrap_or(id.peer);
                let gone = self.lock().take_from(id.peer, from);
                cancel_in_session(session, &gone).await;
            }
            _ => {}
        }
    }
}

async fn cancel_in_session(session: &RwLock<Session>, keys: &[TypingKey]) {
    if keys.is_empty() {
        return;
    }
    let mut session = session.write().await;
    for &(peer, from, top) in keys {
        session.set_typing_action(peer, from, top, TypingAction::Cancel);
    }
}

/// Cancels each action in the session when its time runs out, until the session is gone.
pub(crate) async fn run_typing_expiry(
    state: Arc<TypingExpiryState>,
    session: Weak<RwLock<Session>>,
) {
    loop {
        let next = state.lock().next_deadline();
        match next {
            Some(deadline) => {
                tokio::select! {
                    () = tokio::time::sleep_until(deadline) => {}
                    () = state.changed.notified() => continue,
                }
            }
            None => {
                state.changed.notified().await;
                continue;
            }
        }
        let expired = state.lock().take_expired(Instant::now());
        let Some(session) = session.upgrade() else {
            return;
        };
        cancel_in_session(&session, &expired).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tg_data::{ChatId, FullMsgId, Message, TextWithEntities, UserId};

    fn user(id: u64) -> PeerId {
        PeerId::from_user(UserId(id))
    }

    fn chat() -> PeerId {
        PeerId::from_chat(ChatId(5))
    }

    fn sorted(mut keys: Vec<TypingKey>) -> Vec<TypingKey> {
        keys.sort_by_key(|k| k.1);
        keys
    }

    #[test]
    fn an_action_ends_six_seconds_after_its_last_update_and_a_game_after_ten() {
        let t0 = Instant::now();
        let mut expiry = TypingExpiry::default();
        expiry.record((chat(), user(1), None), TypingAction::Typing, t0);
        expiry.record((chat(), user(2), None), TypingAction::PlayGame, t0);
        assert!(
            expiry
                .take_expired(t0 + Duration::from_millis(5_999))
                .is_empty()
        );
        // Renewed at 4 s: shown until 10 s, like the game.
        expiry.record(
            (chat(), user(1), None),
            TypingAction::Typing,
            t0 + Duration::from_secs(4),
        );
        assert!(expiry.take_expired(t0 + Duration::from_secs(9)).is_empty());
        assert_eq!(
            sorted(expiry.take_expired(t0 + Duration::from_secs(10))),
            vec![(chat(), user(1), None), (chat(), user(2), None)]
        );
        assert_eq!(expiry.next_deadline(), None);
    }

    #[test]
    fn cancel_and_a_message_from_the_sender_end_the_action_at_once() {
        let t0 = Instant::now();
        let mut expiry = TypingExpiry::default();
        expiry.record((chat(), user(1), None), TypingAction::Typing, t0);
        expiry.record((chat(), user(1), Some(MsgId(7))), TypingAction::Typing, t0);
        expiry.record((chat(), user(2), None), TypingAction::RecordVoice, t0);
        expiry.record((chat(), user(2), None), TypingAction::Cancel, t0);
        assert_eq!(
            expiry.take_from(chat(), user(1)).len(),
            2,
            "every topic of the sender"
        );
        assert!(!expiry.has_peer(chat()));
    }

    /// The driver with 300 ms lifetimes: the session's action is cancelled when its time is
    /// up, and a message from the sender cancels it at once.
    #[tokio::test]
    async fn the_session_stops_showing_an_action_when_it_expires_or_the_sender_writes() {
        let session = Arc::new(RwLock::new(Session::new(UserId(100))));
        let state = Arc::new(TypingExpiryState::with_lifetimes(
            Duration::from_millis(300),
            Duration::from_millis(500),
        ));
        let driver = tokio::spawn(run_typing_expiry(
            Arc::clone(&state),
            Arc::downgrade(&session),
        ));
        let typing = |peer, from| ChangeEvent::UserTyping {
            peer_id: peer,
            from_id: from,
            top_msg_id: None,
            action: TypingAction::Typing,
        };

        // 1. Expiry: nobody renews the action.
        session
            .write()
            .await
            .set_typing_action(chat(), user(1), None, TypingAction::Typing);
        state.on_change(&session, &typing(chat(), user(1))).await;
        assert_eq!(session.read().await.active_typing(chat()).len(), 1);
        let started = Instant::now();
        while !session.read().await.active_typing(chat()).is_empty() {
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "the expired action was never cancelled"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            started.elapsed() >= Duration::from_millis(290),
            "shown for its lifetime first, gone after {:?}",
            started.elapsed()
        );

        // 2. A message from the sender ends the action before its time.
        session
            .write()
            .await
            .set_typing_action(chat(), user(2), None, TypingAction::Typing);
        state.on_change(&session, &typing(chat(), user(2))).await;
        let id = FullMsgId::new(chat(), MsgId(50));
        session.write().await.insert_message(Message::new_text(
            chat(),
            MsgId(50),
            Some(user(2)),
            1_750_000_000,
            TextWithEntities::plain("done typing"),
        ));
        state
            .on_change(
                &session,
                &ChangeEvent::MessageUpdated {
                    id,
                    flags: MessageUpdateFlags::NEW_ADDED,
                },
            )
            .await;
        assert!(session.read().await.active_typing(chat()).is_empty());

        drop(session);
        state.changed.notify_one();
        driver.abort();
    }
}
