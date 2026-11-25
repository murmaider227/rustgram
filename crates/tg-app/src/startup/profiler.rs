//! Env-gated startup stage timing, OS memory (`PrivateUsage` / `WorkingSetSize`),
//! and Rust heap allocation counter (`RUSTGRAM_PROFILE_STARTUP=1`).

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

static PROFILE_ENABLED: AtomicBool = AtomicBool::new(false);
static ALLOC_COUNT: AtomicU64 = AtomicU64::new(0);
static DEALLOC_COUNT: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES_TOTAL: AtomicU64 = AtomicU64::new(0);
static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
static START_INSTANT: OnceLock<Instant> = OnceLock::new();
static LAST_MICROS: AtomicU64 = AtomicU64::new(0);

/// Pass-through global allocator that records allocation counts and bytes only when
/// `RUSTGRAM_PROFILE_STARTUP=1` is active.
pub struct ProfilingAllocator;

// SAFETY: Delegates all allocation operations directly to `std::alloc::System`.
unsafe impl GlobalAlloc for ProfilingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: Forwarding directly to `System::alloc`.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() && PROFILE_ENABLED.load(Ordering::Relaxed) {
            let sz = layout.size();
            ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
            ALLOC_BYTES_TOTAL.fetch_add(sz as u64, Ordering::Relaxed);
            LIVE_BYTES.fetch_add(sz, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if PROFILE_ENABLED.load(Ordering::Relaxed) {
            DEALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
            LIVE_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
        }
        // SAFETY: Forwarding directly to `System::dealloc`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: Forwarding directly to `System::alloc_zeroed`.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() && PROFILE_ENABLED.load(Ordering::Relaxed) {
            let sz = layout.size();
            ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
            ALLOC_BYTES_TOTAL.fetch_add(sz as u64, Ordering::Relaxed);
            LIVE_BYTES.fetch_add(sz, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: Forwarding directly to `System::realloc`.
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() && PROFILE_ENABLED.load(Ordering::Relaxed) {
            ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
            ALLOC_BYTES_TOTAL.fetch_add(new_size as u64, Ordering::Relaxed);
            let old_size = layout.size();
            if new_size >= old_size {
                LIVE_BYTES.fetch_add(new_size - old_size, Ordering::Relaxed);
            } else {
                LIVE_BYTES.fetch_sub(old_size - new_size, Ordering::Relaxed);
            }
        }
        new_ptr
    }
}

/// Initializes startup profiling if `RUSTGRAM_PROFILE_STARTUP` is `"1"` or `"true"`.
pub fn init_from_env() {
    let enabled = std::env::var("RUSTGRAM_PROFILE_STARTUP")
        .ok()
        .is_some_and(|v| matches!(v.trim(), "1" | "true" | "TRUE" | "yes"));
    if enabled {
        let _ = START_INSTANT.set(Instant::now());
        LAST_MICROS.store(0, Ordering::Relaxed);
        PROFILE_ENABLED.store(true, Ordering::Relaxed);
        mark("main_enter");
    }
}

/// Returns whether startup profiling is currently enabled.
#[inline]
#[must_use]
pub fn is_enabled() -> bool {
    PROFILE_ENABLED.load(Ordering::Relaxed)
}

/// Emits a structured startup profile line to stderr with elapsed time, delta time,
/// OS private bytes, OS working set, and Rust heap counters.
pub fn mark(stage: &str) {
    if !is_enabled() {
        return;
    }
    let start = START_INSTANT.get_or_init(Instant::now);
    let elapsed_us = start.elapsed().as_micros() as u64;
    let prev_us = LAST_MICROS.swap(elapsed_us, Ordering::Relaxed);
    let delta_us = elapsed_us.saturating_sub(prev_us);
    let (priv_bytes, ws_bytes) = query_process_memory();
    let allocs = ALLOC_COUNT.load(Ordering::Relaxed);
    let deallocs = DEALLOC_COUNT.load(Ordering::Relaxed);
    let total_mib = ALLOC_BYTES_TOTAL.load(Ordering::Relaxed) as f64 / 1_048_576.0;
    let live_mib = LIVE_BYTES.load(Ordering::Relaxed) as f64 / 1_048_576.0;
    eprintln!(
        "[STARTUP_PROF] stage={stage:<34} t_ms={:7.2} dt_ms={:7.2} priv_mib={:6.2} ws_mib={:6.2} allocs={allocs} deallocs={deallocs} total_alloc_mib={total_mib:6.2} live_heap_mib={live_mib:6.2}",
        elapsed_us as f64 / 1000.0,
        delta_us as f64 / 1000.0,
        priv_bytes as f64 / 1_048_576.0,
        ws_bytes as f64 / 1_048_576.0,
    );
}

#[cfg(windows)]
fn query_process_memory() -> (usize, usize) {
    #[repr(C)]
    struct ProcessMemoryCountersEx {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
        private_usage: usize,
    }

    unsafe extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32GetProcessMemoryInfo(
            process: isize,
            ppsmem_counters: *mut ProcessMemoryCountersEx,
            cb: u32,
        ) -> i32;
    }

    let mut pmc = ProcessMemoryCountersEx {
        cb: std::mem::size_of::<ProcessMemoryCountersEx>() as u32,
        page_fault_count: 0,
        peak_working_set_size: 0,
        working_set_size: 0,
        quota_peak_paged_pool_usage: 0,
        quota_paged_pool_usage: 0,
        quota_peak_non_paged_pool_usage: 0,
        quota_non_paged_pool_usage: 0,
        pagefile_usage: 0,
        peak_pagefile_usage: 0,
        private_usage: 0,
    };
    // SAFETY: `GetCurrentProcess` returns the valid current-process pseudo-handle and `pmc` is
    // properly sized with `cb = size_of::<ProcessMemoryCountersEx>()`.
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut pmc, pmc.cb) };
    if ok != 0 {
        (pmc.private_usage, pmc.working_set_size)
    } else {
        (0, 0)
    }
}

#[cfg(not(windows))]
fn query_process_memory() -> (usize, usize) {
    (0, 0)
}

/// Marks the start and end of the first three frames ([`FrameMarks::begin`] in `App::ui`).
pub struct FrameMarks(u64);

impl FrameMarks {
    /// Counts a frame and marks its start while it is one of the first three.
    #[must_use]
    pub fn begin() -> Self {
        static FRAMES: AtomicU64 = AtomicU64::new(0);
        let n = FRAMES.fetch_add(1, Ordering::Relaxed);
        if n < 3 && is_enabled() {
            mark(&format!("frame_{n}_enter"));
        }
        Self(n)
    }
}

impl Drop for FrameMarks {
    fn drop(&mut self) {
        if self.0 < 3 && is_enabled() {
            mark(&format!("frame_{}_exit", self.0));
        }
    }
}
