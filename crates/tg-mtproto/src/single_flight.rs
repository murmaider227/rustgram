//! One in-flight operation per key, shared by every caller that asks for the same key while it runs
//! (the guest-DC auth transfer of `Instance::Private::sendAuthExport` / `exportDone` / `importDone`).
//!
//! The first caller for a key becomes the leader and runs the operation; later callers get a
//! receiver and wait for its result. The result lives in a `watch` channel, so a waiter that polls
//! after the leader has finished still sees it. If the leader is dropped before it finishes (its
//! request was cancelled), the key is freed and waiters learn that there is no result, so one of
//! them can run the operation again.

use std::collections::HashMap;
use std::hash::Hash;

use parking_lot::Mutex;
use tokio::sync::watch;

pub(crate) struct SingleFlight<K, T> {
    in_flight: Mutex<HashMap<K, watch::Receiver<Option<T>>>>,
}

pub(crate) enum Joined<'a, K: Eq + Hash, T> {
    Leader(Leader<'a, K, T>),
    Waiter(Waiter<T>),
}

/// Runs the operation; [`Leader::finish`] publishes its result. Dropping it unfinished frees the key.
pub(crate) struct Leader<'a, K: Eq + Hash, T> {
    flight: &'a SingleFlight<K, T>,
    key: K,
    tx: Option<watch::Sender<Option<T>>>,
}

pub(crate) struct Waiter<T> {
    rx: watch::Receiver<Option<T>>,
}

impl<K: Eq + Hash + Clone, T> SingleFlight<K, T> {
    pub(crate) fn new() -> Self {
        Self {
            in_flight: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn join(&self, key: K) -> Joined<'_, K, T> {
        let mut in_flight = self.in_flight.lock();
        if let Some(rx) = in_flight.get(&key) {
            return Joined::Waiter(Waiter { rx: rx.clone() });
        }
        let (tx, rx) = watch::channel(None);
        in_flight.insert(key.clone(), rx);
        Joined::Leader(Leader {
            flight: self,
            key,
            tx: Some(tx),
        })
    }

    #[cfg(test)]
    fn is_in_flight(&self, key: &K) -> bool {
        self.in_flight.lock().contains_key(key)
    }
}

impl<K: Eq + Hash, T> Leader<'_, K, T> {
    pub(crate) fn finish(mut self, value: T) {
        if let Some(tx) = self.tx.take() {
            self.flight.in_flight.lock().remove(&self.key);
            tx.send_replace(Some(value));
        }
    }
}

impl<K: Eq + Hash, T> Drop for Leader<'_, K, T> {
    fn drop(&mut self) {
        if self.tx.take().is_some() {
            self.flight.in_flight.lock().remove(&self.key);
        }
    }
}

impl<T: Clone> Waiter<T> {
    /// The leader's result, or `None` when the leader was dropped before it finished.
    pub(crate) async fn result(mut self) -> Option<T> {
        self.rx
            .wait_for(Option::is_some)
            .await
            .ok()
            .and_then(|value| value.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leader<'a>(joined: Joined<'a, u8, u32>) -> Leader<'a, u8, u32> {
        match joined {
            Joined::Leader(leader) => leader,
            Joined::Waiter(_) => panic!("expected the leader"),
        }
    }

    fn waiter(joined: Joined<'_, u8, u32>) -> Waiter<u32> {
        match joined {
            Joined::Waiter(waiter) => waiter,
            Joined::Leader(_) => panic!("expected a waiter"),
        }
    }

    #[tokio::test]
    async fn waiter_that_polls_after_the_leader_finished_gets_the_result() {
        let flight = SingleFlight::new();
        let first = leader(flight.join(4));
        let late = waiter(flight.join(4));
        first.finish(7);
        assert_eq!(late.result().await, Some(7));
        assert!(!flight.is_in_flight(&4));
    }

    #[tokio::test]
    async fn waiters_already_polling_wake_on_finish() {
        let flight = SingleFlight::new();
        let first = leader(flight.join(4));
        let waiting = tokio::spawn(waiter(flight.join(4)).result());
        tokio::task::yield_now().await;
        first.finish(9);
        assert_eq!(waiting.await.ok(), Some(Some(9)));
    }

    #[tokio::test]
    async fn dropped_leader_frees_the_key_and_tells_waiters() {
        let flight = SingleFlight::new();
        let first = leader(flight.join(4));
        let pending = waiter(flight.join(4));
        drop(first);
        assert_eq!(pending.result().await, None);
        let _next = leader(flight.join(4));
        assert!(flight.is_in_flight(&4));
    }

    #[tokio::test]
    async fn keys_do_not_share_a_flight() {
        let flight = SingleFlight::new();
        let dc4 = leader(flight.join(4));
        let dc5 = leader(flight.join(5));
        dc5.finish(1);
        assert!(flight.is_in_flight(&4));
        dc4.finish(2);
        assert!(!flight.is_in_flight(&4));
    }
}
