//! Guarded fuzz runs: panics, hangs and huge allocations (TGB-3086).
//!
//! [`Guard::check`] runs each case on a worker thread. A panic is recorded with its location,
//! a case that runs past the timeout is recorded as a hang, and, when the test binary installs
//! [`CountingAlloc`] as its global allocator, a case whose single allocation or live heap goes
//! past the caps is recorded as a huge allocation. A worker that hangs or trips a cap is parked
//! (inside the allocator, before the memory is taken) and replaced, so one bad case neither
//! aborts the process nor eats the machine's memory, and the run goes on.
//!
//! For each finding the shortest input (by its `Debug` text) that hit it is kept, a cheap
//! stand-in for minimisation.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::fmt::Debug;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use crate::{LAST_PANIC, Rng, SILENT, install_hook};

/// Default cap on one allocation inside a case.
pub const DEFAULT_SINGLE_CAP: usize = 256 << 20;
/// Default cap on the live heap a case may grow.
pub const DEFAULT_LIVE_CAP: usize = 512 << 20;
/// Default limit on bytes allocated in total by one case (reported, not stopped).
pub const DEFAULT_TOTAL_LIMIT: usize = 1 << 30;
/// Default per-case timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

const POLL: Duration = Duration::from_millis(20);
const NOT_TRIPPED: usize = 0;
const CANCELLED: usize = usize::MAX;

static INSTALLED: AtomicBool = AtomicBool::new(false);

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
    static LIVE: Cell<usize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
    static TOTAL: Cell<usize> = const { Cell::new(0) };
    static LARGEST: Cell<usize> = const { Cell::new(0) };
    static SINGLE_CAP: Cell<usize> = const { Cell::new(usize::MAX) };
    static LIVE_CAP: Cell<usize> = const { Cell::new(usize::MAX) };
    static TRIP: Cell<*const AtomicUsize> = const { Cell::new(std::ptr::null()) };
}

/// Global allocator that counts the allocations of armed fuzz workers.
///
/// Install it in a fuzz test binary:
/// `#[global_allocator] static ALLOC: tg_fuzz::guard::CountingAlloc = tg_fuzz::guard::CountingAlloc;`
pub struct CountingAlloc;

impl CountingAlloc {
    /// Charges `size` bytes to the armed worker; parks the thread forever when a cap is passed
    /// or the case was cancelled, after reporting it through the worker's trip slot.
    fn charge(size: usize) {
        INSTALLED.store(true, Ordering::Relaxed);
        let Ok(armed) = ARMED.try_with(Cell::get) else {
            return;
        };
        if !armed {
            return;
        }
        let live = LIVE.with(Cell::get).saturating_add(size);
        LIVE.with(|l| l.set(live));
        PEAK.with(|p| p.set(p.get().max(live)));
        TOTAL.with(|t| t.set(t.get().saturating_add(size)));
        LARGEST.with(|l| l.set(l.get().max(size)));
        let trip = TRIP.with(Cell::get);
        if trip.is_null() {
            return;
        }
        // SAFETY: the slot is an `Arc<AtomicUsize>` that the worker thread holds for its whole
        // life (see `Worker::spawn`), so the pointer is valid while this thread runs.
        let slot = unsafe { &*trip };
        let over = size > SINGLE_CAP.with(Cell::get) || live > LIVE_CAP.with(Cell::get);
        if over {
            let _ =
                slot.compare_exchange(NOT_TRIPPED, size.max(1), Ordering::SeqCst, Ordering::SeqCst);
        }
        if over || slot.load(Ordering::SeqCst) == CANCELLED {
            ARMED.with(|a| a.set(false));
            loop {
                std::thread::park();
            }
        }
    }

    fn release(size: usize) {
        if ARMED.try_with(Cell::get).unwrap_or(false) {
            LIVE.with(|l| l.set(l.get().saturating_sub(size)));
        }
    }
}

// SAFETY: every call is forwarded to `System` unchanged; the counters only use const-initialised
// thread locals without destructors, which do not allocate.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::charge(layout.size());
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::charge(layout.size());
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        Self::release(layout.size());
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::charge(new_size);
        Self::release(layout.size());
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// What one case did.
#[derive(Debug, Clone, Default)]
struct Outcome {
    panic_at: Option<String>,
    total: usize,
    peak: usize,
}

type Job = Box<dyn FnOnce() + Send>;

struct Worker {
    jobs: Sender<Job>,
    results: Receiver<Outcome>,
    trip: Arc<AtomicUsize>,
}

impl Worker {
    fn spawn(single_cap: usize, live_cap: usize) -> Option<Self> {
        let (jobs, job_rx) = channel::<Job>();
        let (result_tx, results) = channel::<Outcome>();
        let trip = Arc::new(AtomicUsize::new(NOT_TRIPPED));
        let held = Arc::clone(&trip);
        std::thread::Builder::new()
            .name("tg-fuzz-worker".into())
            .stack_size(16 << 20)
            .spawn(move || {
                SILENT.with(|s| *s.borrow_mut() = true);
                SINGLE_CAP.with(|c| c.set(single_cap));
                LIVE_CAP.with(|c| c.set(live_cap));
                TRIP.with(|t| t.set(Arc::as_ptr(&held)));
                while let Ok(job) = job_rx.recv() {
                    LIVE.with(|l| l.set(0));
                    PEAK.with(|p| p.set(0));
                    TOTAL.with(|t| t.set(0));
                    LARGEST.with(|l| l.set(0));
                    LAST_PANIC.with(|p| p.borrow_mut().clear());
                    ARMED.with(|a| a.set(true));
                    let result = catch_unwind(AssertUnwindSafe(job));
                    ARMED.with(|a| a.set(false));
                    let outcome = Outcome {
                        panic_at: result
                            .is_err()
                            .then(|| LAST_PANIC.with(|p| p.borrow().clone())),
                        total: TOTAL.with(Cell::get),
                        peak: PEAK.with(Cell::get),
                    };
                    if result_tx.send(outcome).is_err() {
                        break;
                    }
                }
                TRIP.with(|t| t.set(std::ptr::null()));
                drop(held);
            })
            .ok()?;
        Some(Self {
            jobs,
            results,
            trip,
        })
    }
}

/// Run statistics, for the test's summary line.
#[derive(Debug, Clone, Copy, Default)]
pub struct GuardStats {
    /// Cases run.
    pub cases: usize,
    /// Largest total allocation of one case, in bytes (0 without [`CountingAlloc`]).
    pub max_total: usize,
    /// Largest live heap of one case, in bytes (0 without [`CountingAlloc`]).
    pub max_peak: usize,
    /// Slowest case.
    pub slowest: Duration,
}

/// A guarded fuzz run with a fixed seed.
pub struct Guard {
    rng: Rng,
    iterations: usize,
    timeout: Duration,
    single_cap: usize,
    live_cap: usize,
    total_limit: usize,
    worker: Option<Worker>,
    /// Finding key -> shortest description seen.
    findings: BTreeMap<String, String>,
    stats: GuardStats,
}

fn env_usize(name: &str) -> Option<usize> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}

impl Guard {
    /// Creates a run. The iteration count defaults to `default_iterations`;
    /// `TGFORK_FUZZ_ITERATIONS` overrides it and `TGFORK_FUZZ_TIMEOUT_MS` the per-case timeout.
    pub fn new(seed: u64, default_iterations: usize) -> Self {
        install_hook();
        Self {
            rng: Rng::new(seed),
            iterations: env_usize("TGFORK_FUZZ_ITERATIONS").unwrap_or(default_iterations),
            timeout: env_usize("TGFORK_FUZZ_TIMEOUT_MS")
                .map_or(DEFAULT_TIMEOUT, |ms| Duration::from_millis(ms as u64)),
            single_cap: DEFAULT_SINGLE_CAP,
            live_cap: DEFAULT_LIVE_CAP,
            total_limit: DEFAULT_TOTAL_LIMIT,
            worker: None,
            findings: BTreeMap::new(),
            stats: GuardStats::default(),
        }
    }

    /// Overrides the per-case timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Overrides the allocation caps (single allocation, live heap) and the reported total.
    pub fn with_alloc_limits(mut self, single: usize, live: usize, total: usize) -> Self {
        self.single_cap = single;
        self.live_cap = live;
        self.total_limit = total;
        self
    }

    /// Number of iterations the caller should run.
    pub fn iterations(&self) -> usize {
        self.iterations
    }

    /// The generator.
    pub fn rng(&mut self) -> &mut Rng {
        &mut self.rng
    }

    /// Statistics so far.
    pub fn stats(&self) -> GuardStats {
        self.stats
    }

    /// Whether [`CountingAlloc`] is the global allocator of this binary (known after the first
    /// allocation).
    pub fn counting_allocations() -> bool {
        INSTALLED.load(Ordering::Relaxed)
    }

    /// Runs `case` on the worker; records a panic, hang or huge allocation under `name` with
    /// the iteration and `input`.
    pub fn check<T, F>(&mut self, name: &str, iteration: usize, input: T, case: F)
    where
        T: Debug + Send + Sync + 'static,
        F: FnOnce(&T) + Send + 'static,
    {
        if self.worker.is_none() {
            self.worker = Worker::spawn(self.single_cap, self.live_cap);
        }
        let Some(worker) = &self.worker else {
            self.record(
                format!("{name}: cannot start a worker thread"),
                String::new(),
            );
            return;
        };
        let shared = Arc::new(input);
        let for_job = Arc::clone(&shared);
        let started = Instant::now();
        if worker.jobs.send(Box::new(move || case(&for_job))).is_err() {
            self.worker = None;
            return;
        }
        let result = loop {
            let left = self.timeout.saturating_sub(started.elapsed());
            match worker.results.recv_timeout(left.min(POLL)) {
                Ok(outcome) => break Some(outcome),
                Err(RecvTimeoutError::Disconnected) => break None,
                Err(RecvTimeoutError::Timeout) => {
                    if left <= POLL || worker.trip.load(Ordering::SeqCst) != NOT_TRIPPED {
                        break None;
                    }
                }
            }
        };
        self.stats.cases += 1;
        self.stats.slowest = self.stats.slowest.max(started.elapsed());
        let describe = || format!("{name} at iteration {iteration} on {:?}", *shared);
        let Some(outcome) = result else {
            let tripped = worker.trip.swap(CANCELLED, Ordering::SeqCst);
            let key = if tripped == NOT_TRIPPED {
                format!("{name}: no result after {:?} (hang?)", self.timeout)
            } else {
                format!("{name}: huge allocation ({} MiB requested)", tripped >> 20)
            };
            self.record(key, describe());
            // The old worker is parked (or still spinning and parks at its next allocation);
            // it is abandoned and a new one starts with the next case.
            self.worker = None;
            return;
        };
        self.stats.max_total = self.stats.max_total.max(outcome.total);
        self.stats.max_peak = self.stats.max_peak.max(outcome.peak);
        if let Some(at) = outcome.panic_at {
            self.record(format!("panic at {at}"), describe());
        } else if outcome.total > self.total_limit {
            self.record(
                format!("{name}: allocated {} MiB in one call", outcome.total >> 20),
                describe(),
            );
        }
    }

    fn record(&mut self, key: String, what: String) {
        let what = shorten(what, 4000);
        match self.findings.get_mut(&key) {
            Some(old) if old.len() <= what.len() => {}
            Some(old) => *old = what,
            None => {
                self.findings.insert(key, what);
            }
        }
    }

    /// Distinct findings so far (key, shortest description).
    pub fn findings(&self) -> impl Iterator<Item = (&str, &str)> {
        self.findings.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// Fails the test with every distinct finding.
    pub fn finish(self) {
        let report: Vec<String> = self
            .findings
            .iter()
            .map(|(at, what)| format!("{at}\n    {what}"))
            .collect();
        assert!(report.is_empty(), "fuzz findings:\n{}", report.join("\n"));
    }
}

fn shorten(mut s: String, max: usize) -> String {
    if s.len() > max {
        let cut = (0..=max)
            .rev()
            .find(|&i| s.is_char_boundary(i))
            .unwrap_or(0);
        s.truncate(cut);
        s.push_str("...");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_panics_and_keeps_the_shortest_input() {
        let mut guard = Guard::new(1, 1);
        for (i, input) in ["long input here", "x"].into_iter().enumerate() {
            guard.check("boom", i, input.to_string(), |s| {
                let v: Vec<u8> = Vec::new();
                let _ = v[s.len()];
            });
        }
        let findings: Vec<_> = guard.findings().collect();
        assert_eq!(findings.len(), 1);
        assert!(findings[0].1.contains("\"x\""), "{findings:?}");
    }

    #[test]
    fn records_a_hang_and_recovers() {
        let mut guard = Guard::new(1, 1).with_timeout(Duration::from_millis(100));
        guard.check("spin", 0, (), |_| {
            let stop = Instant::now() + Duration::from_millis(400);
            while Instant::now() < stop {
                std::hint::spin_loop();
            }
        });
        guard.check("fine", 1, (), |_| {});
        let keys: Vec<_> = guard.findings().map(|(k, _)| k.to_string()).collect();
        assert_eq!(keys.len(), 1, "{keys:?}");
        assert!(keys[0].contains("spin"), "{keys:?}");
        assert_eq!(guard.stats().cases, 2);
    }
}
