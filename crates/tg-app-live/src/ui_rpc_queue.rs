//! Ordered background queue for requests the UI starts but does not wait for: typing
//! (`messages.setTyping`), read receipts (`messages.readHistory`) and cloud drafts
//! (`messages.saveDraft`).
//!
//! tdesktop sends these asynchronously (`Api::SendProgressManager`,
//! `Data::Histories::readInboxTill`, `ApiWrap::saveDraftToCloudDelayed`) and applies the local
//! effect at once. The queue runs its jobs one after another on the session runtime, so the
//! server sees them in the order the user caused them (a typing cancel never overtakes the typing
//! it cancels), while the UI thread returns immediately.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use tokio::runtime::Handle;
use tokio::sync::mpsc;

type Job = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Counters shared between the queue handle and its worker.
#[derive(Debug, Default)]
struct Progress {
    queued: AtomicU64,
    finished: Mutex<u64>,
    job_done: Condvar,
}

impl Progress {
    fn finish_one(&self) {
        if let Ok(mut finished) = self.finished.lock() {
            *finished += 1;
        }
        self.job_done.notify_all();
    }
}

/// Handle to one session's ordered request queue; clones share the worker.
#[derive(Clone)]
pub(crate) struct UiRpcQueue {
    tx: mpsc::UnboundedSender<Job>,
    progress: Arc<Progress>,
}

impl UiRpcQueue {
    /// Starts the worker on `handle`. It ends when every handle is dropped and the queued jobs
    /// have run.
    pub(crate) fn start(handle: &Handle) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<Job>();
        let progress = Arc::new(Progress::default());
        let worker_progress = Arc::clone(&progress);
        handle.spawn(async move {
            while let Some(job) = rx.recv().await {
                job.await;
                worker_progress.finish_one();
            }
        });
        Self { tx, progress }
    }

    /// Queues `job` after every job queued before it. Returns `false` when the worker is gone
    /// (runtime shut down); the job is dropped then.
    pub(crate) fn push(&self, job: impl Future<Output = ()> + Send + 'static) -> bool {
        self.progress.queued.fetch_add(1, Ordering::AcqRel);
        if self.tx.send(Box::pin(job)).is_ok() {
            true
        } else {
            self.progress.finish_one();
            false
        }
    }

    /// `true` when every queued job has run.
    #[cfg(test)]
    pub(crate) fn is_idle(&self) -> bool {
        let queued = self.progress.queued.load(Ordering::Acquire);
        self.progress
            .finished
            .lock()
            .is_ok_and(|finished| *finished >= queued)
    }

    /// Waits (blocking, with a time limit) until every job queued so far has run. Only for tests
    /// and shutdown paths: the UI never waits for the queue.
    pub(crate) fn wait_idle(&self, limit: Duration) -> bool {
        let target = self.progress.queued.load(Ordering::Acquire);
        let Ok(finished) = self.progress.finished.lock() else {
            return false;
        };
        self.progress
            .job_done
            .wait_timeout_while(finished, limit, |finished| *finished < target)
            .is_ok_and(|(finished, _)| *finished >= target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn jobs_run_in_the_order_they_were_queued_even_when_earlier_ones_are_slower() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime");
        let queue = UiRpcQueue::start(rt.handle());
        let order = Arc::new(Mutex::new(Vec::new()));
        for i in 0..50u64 {
            let order = Arc::clone(&order);
            // Earlier jobs sleep longer: a pool of spawned tasks would finish them last.
            let delay = Duration::from_micros((50 - i) * 100);
            assert!(queue.push(async move {
                tokio::time::sleep(delay).await;
                if let Ok(mut o) = order.lock() {
                    o.push(i);
                }
            }));
        }
        assert!(queue.wait_idle(Duration::from_secs(10)));
        assert!(queue.is_idle());
        let got = order.lock().map(|o| o.clone()).unwrap_or_default();
        assert_eq!(got, (0..50).collect::<Vec<_>>());
    }

    #[test]
    fn push_returns_immediately_while_a_job_is_still_waiting() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("runtime");
        let queue = UiRpcQueue::start(rt.handle());
        let started = Instant::now();
        queue.push(async { tokio::time::sleep(Duration::from_millis(300)).await });
        queue.push(async {});
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "queuing must not wait for the request"
        );
        assert!(!queue.is_idle());
        assert!(queue.wait_idle(Duration::from_secs(10)));
    }
}
