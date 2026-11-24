//! Runs one blocking OS call (a Windows Hello consent prompt, a WinRT geolocation fix) on a
//! worker thread so the egui frame loop keeps painting and pumping window messages meanwhile.
//!
//! tdesktop answers these calls through callbacks on the main thread (`crl::on_main` in
//! `PasscodeLockWidget::suggestSystemUnlock`, `Platform::ResolveCurrentExactLocation`); here the
//! UI polls [`PlatformCall::poll`] once per frame instead.

use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// How often a frame with a call in flight asks egui to repaint, in case the worker could not.
pub const PLATFORM_CALL_REPAINT_INTERVAL: Duration = Duration::from_millis(100);

/// The state of a [`PlatformCall`] seen by one poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlatformCallState<T> {
    /// The worker is still running.
    Pending,
    /// The worker finished with this value (returned once).
    Done(T),
    /// The worker panicked or could not be started; no value will arrive.
    Lost,
}

/// One blocking platform call running on its own thread.
///
/// Clones share the same call, so state structs that derive `Clone` can hold one.
#[derive(Debug)]
pub struct PlatformCall<T> {
    rx: Arc<Mutex<Receiver<T>>>,
}

impl<T> Clone for PlatformCall<T> {
    fn clone(&self) -> Self {
        Self {
            rx: Arc::clone(&self.rx),
        }
    }
}

impl<T: Send + 'static> PlatformCall<T> {
    /// Creates an already-completed [`PlatformCall`] without spawning a worker thread.
    #[must_use]
    pub fn ready(value: T) -> Self {
        let (tx, rx) = sync_channel(1);
        let _ = tx.send(value);
        Self {
            rx: Arc::new(Mutex::new(rx)),
        }
    }

    /// Wraps an externally-driven single-shot [`Receiver`] (for example a `tokio` background task
    /// that completes an async RPC and calls `egui::Context::request_repaint`).
    #[must_use]
    pub fn from_receiver(rx: Receiver<T>) -> Self {
        Self {
            rx: Arc::new(Mutex::new(rx)),
        }
    }

    /// Starts `work` on a new thread named `tg-<label>`.
    ///
    /// When `repaint` is given, the worker asks egui for a new frame as soon as the value is ready.
    pub fn spawn<F>(label: &str, repaint: Option<egui::Context>, work: F) -> Self
    where
        F: FnOnce() -> T + Send + 'static,
    {
        let (tx, rx) = sync_channel(1);
        let spawned = std::thread::Builder::new()
            .name(format!("tg-{label}"))
            .spawn(move || {
                let value = work();
                let _ = tx.send(value);
                if let Some(ctx) = repaint {
                    ctx.request_repaint();
                }
            });
        if let Err(err) = spawned {
            tracing::warn!(%err, label, "Failed to start platform call thread");
        }
        Self {
            rx: Arc::new(Mutex::new(rx)),
        }
    }

    /// Returns the value once it is ready, without blocking.
    pub fn poll(&self) -> PlatformCallState<T> {
        let rx = self.rx.lock().unwrap_or_else(PoisonError::into_inner);
        match rx.try_recv() {
            Ok(value) => PlatformCallState::Done(value),
            Err(TryRecvError::Empty) => PlatformCallState::Pending,
            Err(TryRecvError::Disconnected) => PlatformCallState::Lost,
        }
    }

    /// Blocks up to `timeout` for the value (tests and shutdown paths only, never a UI frame).
    pub fn wait(&self, timeout: Duration) -> PlatformCallState<T> {
        let rx = self.rx.lock().unwrap_or_else(PoisonError::into_inner);
        match rx.recv_timeout(timeout) {
            Ok(value) => PlatformCallState::Done(value),
            Err(RecvTimeoutError::Timeout) => PlatformCallState::Pending,
            Err(RecvTimeoutError::Disconnected) => PlatformCallState::Lost,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    #[test]
    fn poll_is_pending_until_worker_finishes_then_done_once() {
        let gate = Arc::new(Barrier::new(2));
        let worker_gate = Arc::clone(&gate);
        let call = PlatformCall::spawn("test-call", None, move || {
            worker_gate.wait();
            7_u32
        });
        assert_eq!(call.poll(), PlatformCallState::Pending);
        gate.wait();
        assert_eq!(
            call.wait(Duration::from_secs(5)),
            PlatformCallState::Done(7)
        );
        assert_eq!(call.poll(), PlatformCallState::Lost);
    }

    #[test]
    fn panicking_worker_reports_lost() {
        let call: PlatformCall<u32> =
            PlatformCall::spawn("test-panic", None, || panic!("worker failed on purpose"));
        assert_eq!(call.wait(Duration::from_secs(5)), PlatformCallState::Lost);
    }
}
