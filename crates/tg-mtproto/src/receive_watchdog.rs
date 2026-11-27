//! Receive watchdog of a session connection: port of `SessionPrivate::onSentSome`,
//! `onReceivedSome`, `markConnectionOld` and `waitReceivedFailed`
//! (`mtproto/session_private.cpp`, v7.4.7 with 805bd26).
//!
//! After the client sends anything that needs a reply, the connection must receive some data
//! within a window. The window starts at [`MIN_RECEIVE_TIMEOUT`], grows with the bytes the
//! reply may wait behind (8 KB/s, so 512 KB give 64 s), doubles after every miss and is capped
//! at [`MAX_RECEIVE_TIMEOUT`]. When it runs out the session drops the connection and dials
//! again at once, so a connection that went silent (network change, sleep, a dead NAT mapping)
//! is replaced in seconds instead of waiting for the OS to fail the socket.
//!
//! Times are [`tokio::time::Instant`] so tests can run it on a paused clock.

use std::time::Duration;

use tg_transport::Error as TransportError;
use tokio::time::Instant;

use crate::transport_adapter::{BoxFuture, PacketConnection};

/// Smallest receive window (`kMinReceiveTimeout = 4000ms`).
pub const MIN_RECEIVE_TIMEOUT: Duration = Duration::from_secs(4);
/// Largest receive window (`kMaxReceiveTimeout = 64000ms`).
pub const MAX_RECEIVE_TIMEOUT: Duration = Duration::from_secs(64);
/// A connection that received nothing for this long is "old": its window drops back to
/// [`MIN_RECEIVE_TIMEOUT`] and stops growing with the request size
/// (`kMarkConnectionOldTimeout = 192000ms`).
pub const MARK_CONNECTION_OLD_TIMEOUT: Duration = Duration::from_secs(192);
/// Reply speed the window assumes: 8 KB per second of window (`onSentSome`).
const ASSUMED_BYTES_PER_WINDOW: u64 = 8192;

/// Round trip of the first reply on a connection (`_firstSentAt`: `0`, a time, or `-1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FirstReply {
    /// Nothing that needs a reply was sent on this connection yet.
    NotSent,
    /// The first packet that needs a reply went out at this time.
    SentAt(Instant),
    /// The first reply arrived (or there is no connection); stop measuring.
    Measured,
}

/// Receive watchdog state. One per session; it outlives connections because the learned
/// window carries over to the next connection, as `_waitForReceived` does in tdesktop.
#[derive(Debug, Clone)]
pub struct ReceiveWatchdog {
    wait_for_received: Duration,
    deadline: Option<Instant>,
    first_reply: FirstReply,
    old_connection: bool,
    mark_old_at: Option<Instant>,
}

impl Default for ReceiveWatchdog {
    fn default() -> Self {
        Self::new()
    }
}

impl ReceiveWatchdog {
    /// Creates a watchdog with the minimal window and no connection.
    pub const fn new() -> Self {
        Self {
            wait_for_received: MIN_RECEIVE_TIMEOUT,
            deadline: None,
            first_reply: FirstReply::Measured,
            old_connection: true,
            mark_old_at: None,
        }
    }

    /// Current base window (`_waitForReceived`).
    pub const fn wait_for_received(&self) -> Duration {
        self.wait_for_received
    }

    /// When the connection counts as silent, if a reply is awaited.
    pub const fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Whether the connection received nothing for [`MARK_CONNECTION_OLD_TIMEOUT`].
    pub fn is_old(&mut self, now: Instant) -> bool {
        self.apply_mark_old(now);
        self.old_connection
    }

    /// A new connection was created (`SessionPrivate::createDcData` / `connectToServer`).
    pub fn connection_started(&mut self, now: Instant) {
        self.deadline = None;
        self.first_reply = FirstReply::NotSent;
        self.old_connection = false;
        self.mark_old_at = Some(now + MARK_CONNECTION_OLD_TIMEOUT);
    }

    /// The connection is gone; no reply is awaited on it any more.
    pub fn connection_closed(&mut self) {
        self.deadline = None;
        self.mark_old_at = None;
        self.first_reply = FirstReply::Measured;
    }

    /// A packet of `size` bytes that needs a reply was sent (`onSentSome`). `unanswered` is the
    /// size of the requests still waiting for delivery that this reply may queue behind
    /// (805bd26 `haveSentBytes`); pass `0` when the packet does not queue behind them.
    pub fn on_sent_some(&mut self, now: Instant, size: u64, unanswered: u64) {
        self.apply_mark_old(now);
        if self.deadline.is_none() {
            let mut remain = self.wait_for_received;
            if !self.old_connection {
                let size = size.max(unanswered);
                let base_ms = duration_ms(self.wait_for_received);
                let by_size_ms = size.saturating_mul(base_ms) / ASSUMED_BYTES_PER_WINDOW;
                remain = Duration::from_millis(by_size_ms)
                    .clamp(self.wait_for_received, MAX_RECEIVE_TIMEOUT);
            }
            self.deadline = Some(now + remain);
        }
        if self.first_reply == FirstReply::NotSent {
            self.first_reply = FirstReply::SentAt(now);
        }
    }

    /// Some data arrived on the connection (`onReceivedSome`): cancel the deadline and, for the
    /// first reply on this connection, shrink the window to twice its round trip.
    pub fn on_received_some(&mut self, now: Instant) {
        self.apply_mark_old(now);
        self.old_connection = false;
        self.mark_old_at = Some(now + MARK_CONNECTION_OLD_TIMEOUT);
        self.deadline = None;
        if let FirstReply::SentAt(sent_at) = self.first_reply {
            let ms = duration_ms(now.saturating_duration_since(sent_at));
            let wait_ms = duration_ms(self.wait_for_received);
            if ms > 0 && ms.saturating_mul(2) < wait_ms {
                self.wait_for_received = Duration::from_millis(ms * 2).max(MIN_RECEIVE_TIMEOUT);
            }
            self.first_reply = FirstReply::Measured;
        }
    }

    /// The deadline passed with nothing received (`waitReceivedFailed`): double the window up
    /// to [`MAX_RECEIVE_TIMEOUT`]. The caller drops the connection and dials again at once.
    pub fn on_wait_failed(&mut self) {
        self.wait_for_received = (self.wait_for_received * 2).min(MAX_RECEIVE_TIMEOUT);
        self.connection_closed();
    }

    /// `_oldConnectionTimer` (`markConnectionOld`), evaluated lazily: the window is only read
    /// by the calls above, so applying the mark on the next call gives the same result.
    fn apply_mark_old(&mut self, now: Instant) {
        if let Some(at) = self.mark_old_at
            && now >= at
        {
            self.mark_old_at = None;
            self.old_connection = true;
            self.wait_for_received = MIN_RECEIVE_TIMEOUT;
        }
    }
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Connection wrapper that runs the watchdog over auth key creation: every DH packet needs a
/// reply, and tdesktop's key creator reports its sends to `onSentSome` the same way
/// (`delegate.sentSome` in `SessionPrivate::tryAcquireKeyCreation`). A reply that does not
/// arrive in time fails `recv` with [`TransportError::Timeout`].
pub(crate) struct WatchedConnection<'a> {
    inner: &'a mut dyn PacketConnection,
    watchdog: &'a mut ReceiveWatchdog,
    timed_out: bool,
}

impl<'a> WatchedConnection<'a> {
    pub(crate) fn new(
        inner: &'a mut dyn PacketConnection,
        watchdog: &'a mut ReceiveWatchdog,
    ) -> Self {
        Self {
            inner,
            watchdog,
            timed_out: false,
        }
    }

    /// Whether a reply did not arrive within the watchdog window.
    pub(crate) const fn timed_out(&self) -> bool {
        self.timed_out
    }
}

impl PacketConnection for WatchedConnection<'_> {
    fn send<'b>(&'b mut self, payload: &'b [u8]) -> BoxFuture<'b, Result<(), TransportError>> {
        Box::pin(async move {
            self.inner.send(payload).await?;
            self.watchdog
                .on_sent_some(Instant::now(), payload.len() as u64, 0);
            Ok(())
        })
    }

    fn recv(&mut self) -> BoxFuture<'_, Result<Vec<u8>, TransportError>> {
        Box::pin(async move {
            loop {
                let Some(deadline) = self.watchdog.deadline() else {
                    let packet = self.inner.recv().await?;
                    self.watchdog.on_received_some(Instant::now());
                    return Ok(packet);
                };
                let seen = self.inner.received_bytes();
                if let Ok(res) = tokio::time::timeout_at(deadline, self.inner.recv()).await {
                    let packet = res?;
                    self.watchdog.on_received_some(Instant::now());
                    return Ok(packet);
                }
                if self.inner.received_bytes() != seen {
                    // Part of a packet arrived: the connection is alive.
                    self.watchdog.on_received_some(Instant::now());
                    continue;
                }
                self.watchdog.on_wait_failed();
                self.timed_out = true;
                return Err(TransportError::Timeout);
            }
        })
    }

    fn close(&mut self) -> BoxFuture<'_, ()> {
        self.inner.close()
    }

    fn using_http_wait(&self) -> bool {
        self.inner.using_http_wait()
    }

    fn need_http_wait(&self) -> bool {
        self.inner.need_http_wait()
    }

    fn transport_tag(&self) -> &str {
        self.inner.transport_tag()
    }

    fn received_bytes(&self) -> u64 {
        self.inner.received_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started() -> (ReceiveWatchdog, Instant) {
        let now = Instant::now();
        let mut watchdog = ReceiveWatchdog::new();
        watchdog.connection_started(now);
        (watchdog, now)
    }

    #[test]
    fn small_request_waits_the_minimal_window() {
        let (mut watchdog, now) = started();
        watchdog.on_sent_some(now, 120, 0);
        assert_eq!(watchdog.deadline(), Some(now + MIN_RECEIVE_TIMEOUT));
    }

    #[test]
    fn window_grows_with_size_and_is_capped() {
        let (mut watchdog, now) = started();
        // 64 KB at 8 KB per window: 8 windows of 4 s.
        watchdog.on_sent_some(now, 64 * 1024, 0);
        assert_eq!(watchdog.deadline(), Some(now + Duration::from_secs(32)));

        let (mut watchdog, now) = started();
        watchdog.on_sent_some(now, 512 * 1024 * 4, 0);
        assert_eq!(watchdog.deadline(), Some(now + MAX_RECEIVE_TIMEOUT));
    }

    /// 805bd26: a small request queued behind a large unanswered upload gets the window of
    /// the bytes in front of it, not the 4 s of its own size.
    #[test]
    fn window_counts_unanswered_bytes_in_front() {
        let (mut watchdog, now) = started();
        watchdog.on_sent_some(now, 100, 256 * 1024);
        assert_eq!(watchdog.deadline(), Some(now + MAX_RECEIVE_TIMEOUT));
    }

    #[test]
    fn later_sends_do_not_extend_a_running_deadline() {
        let (mut watchdog, now) = started();
        watchdog.on_sent_some(now, 100, 0);
        watchdog.on_sent_some(now + Duration::from_secs(3), 512 * 1024, 0);
        assert_eq!(watchdog.deadline(), Some(now + MIN_RECEIVE_TIMEOUT));
    }

    #[test]
    fn misses_double_the_window_up_to_the_cap() {
        let (mut watchdog, _) = started();
        let mut seen = Vec::new();
        for _ in 0..6 {
            watchdog.on_wait_failed();
            seen.push(watchdog.wait_for_received().as_secs());
        }
        assert_eq!(seen, [8, 16, 32, 64, 64, 64]);
        assert_eq!(watchdog.deadline(), None);
    }

    #[test]
    fn first_reply_shrinks_the_window_to_twice_the_round_trip() {
        let (mut watchdog, now) = started();
        for _ in 0..3 {
            watchdog.on_wait_failed();
        }
        assert_eq!(watchdog.wait_for_received(), Duration::from_secs(32));
        watchdog.connection_started(now);
        watchdog.on_sent_some(now, 100, 0);
        watchdog.on_received_some(now + Duration::from_secs(5));
        assert_eq!(watchdog.wait_for_received(), Duration::from_secs(10));
        assert_eq!(watchdog.deadline(), None);

        // Only the first reply on a connection is measured.
        let later = now + Duration::from_secs(20);
        watchdog.on_sent_some(later, 100, 0);
        watchdog.on_received_some(later + Duration::from_millis(100));
        assert_eq!(watchdog.wait_for_received(), Duration::from_secs(10));

        // A fast first reply never drops the window below the minimum.
        watchdog.connection_started(later);
        watchdog.on_sent_some(later, 100, 0);
        watchdog.on_received_some(later + Duration::from_millis(300));
        assert_eq!(watchdog.wait_for_received(), MIN_RECEIVE_TIMEOUT);
    }

    #[test]
    fn quiet_connection_turns_old_and_resets_the_window() {
        let (mut watchdog, now) = started();
        watchdog.on_wait_failed();
        watchdog.connection_started(now);
        assert_eq!(watchdog.wait_for_received(), Duration::from_secs(8));
        let quiet = now + MARK_CONNECTION_OLD_TIMEOUT;
        assert!(watchdog.is_old(quiet));
        assert_eq!(watchdog.wait_for_received(), MIN_RECEIVE_TIMEOUT);
        // An old connection does not size the window by the request.
        watchdog.on_sent_some(quiet, 512 * 1024, 0);
        assert_eq!(watchdog.deadline(), Some(quiet + MIN_RECEIVE_TIMEOUT));
        // Receiving makes it fresh again.
        watchdog.on_received_some(quiet + Duration::from_secs(1));
        assert!(!watchdog.is_old(quiet + Duration::from_secs(2)));
    }
}
