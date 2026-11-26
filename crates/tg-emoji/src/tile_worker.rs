//! Two long-lived threads that decode and upload emoji tiles off the paint path.
//!
//! A tile decodes in about 1 ms, so a small fixed pool keeps up with a frame that asks for many
//! new tiles at once (opening the emoji panel) without starting a thread per tile.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// Work item run on a tile worker.
pub(crate) type Job = Box<dyn FnOnce() + Send + 'static>;

const WORKERS: usize = 2;

fn queue() -> Option<&'static Sender<Job>> {
    static QUEUE: OnceLock<Option<Sender<Job>>> = OnceLock::new();
    QUEUE
        .get_or_init(|| {
            let (sender, receiver) = mpsc::channel::<Job>();
            let receiver = Arc::new(Mutex::new(receiver));
            let started = (0..WORKERS)
                .filter(|worker| {
                    let receiver = Arc::clone(&receiver);
                    std::thread::Builder::new()
                        .name(format!("tg-emoji-tile-{worker}"))
                        .spawn(move || run_worker(&receiver))
                        .is_ok()
                })
                .count();
            (started > 0).then_some(sender)
        })
        .as_ref()
}

fn run_worker(receiver: &Mutex<Receiver<Job>>) {
    loop {
        let job = receiver
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .recv();
        let Ok(job) = job else {
            return;
        };
        // A panicking job must not take the worker down with it; the job's own drop guard
        // reports the failure to whoever waits for it.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
    }
}

/// Returns `true` when at least one tile worker thread is running.
pub(crate) fn available() -> bool {
    queue().is_some()
}

/// Queues `job` on the tile workers; hands it back when no worker could be started.
pub(crate) fn submit(job: Job) -> Result<(), Job> {
    match queue() {
        Some(sender) => sender.send(job).map_err(|err| err.0),
        None => Err(job),
    }
}
