//! Port of `lib_base/base/unixtime.cpp`: server time offset correction, HTTP Date unixtime
//! synchronization, and monotonic MTProto `msg_id` generation (time-based, strictly increasing,
//! always divisible by 4).

use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;

/// Differences smaller than 3 seconds are ignored on non-forced updates (`kIgnoreTimeDifference`).
pub const IGNORE_TIME_DIFFERENCE_SECS: i64 = 3;

#[derive(Debug)]
struct ClockInner {
    shift_secs: i64,
    updated: bool,
    http_shift_secs: i64,
    http_updated: bool,
    last_msg_id: u64,
    start_instant: Instant,
    start_unix_secs: i64,
}

/// Shared MTProto clock & monotonic `msg_id` generator.
#[derive(Debug, Clone)]
pub struct MtprotoClock {
    inner: Arc<Mutex<ClockInner>>,
}

impl Default for MtprotoClock {
    fn default() -> Self {
        Self::new()
    }
}

impl MtprotoClock {
    /// Creates a new clock synchronized to local system time.
    pub fn new() -> Self {
        let now_secs = local_unix_secs();
        Self {
            inner: Arc::new(Mutex::new(ClockInner {
                shift_secs: 0,
                updated: false,
                http_shift_secs: 0,
                http_updated: false,
                last_msg_id: 0,
                start_instant: Instant::now(),
                start_unix_secs: now_secs,
            })),
        }
    }

    /// Creates a clock with a fixed initial time offset (for deterministic tests).
    pub fn with_shift(shift_secs: i64) -> Self {
        let clock = Self::new();
        {
            let mut guard = clock.inner.lock();
            guard.shift_secs = shift_secs;
            guard.start_unix_secs = local_unix_secs() + shift_secs;
        }
        clock
    }

    /// Current corrected server unix time in seconds (`base::unixtime::now`).
    pub fn now_secs(&self) -> i32 {
        let guard = self.inner.lock();
        (local_unix_secs() + guard.shift_secs) as i32
    }

    /// Current time shift (`server_time - local_time`) in seconds.
    pub fn time_shift_secs(&self) -> i64 {
        self.inner.lock().shift_secs
    }

    /// Updates the server time offset (`base::unixtime::update`).
    ///
    /// Returns `true` if the offset actually changed.
    pub fn update_server_time(&self, server_time: i32, force: bool) -> bool {
        let mut guard = self.inner.lock();
        if !force && guard.updated {
            return false;
        }
        let local = local_unix_secs();
        let new_shift = (server_time as i64) - local;
        if !force && (new_shift - guard.shift_secs).abs() < IGNORE_TIME_DIFFERENCE_SECS {
            return false;
        }
        guard.updated = true;
        if guard.shift_secs == new_shift {
            // Even if shift_secs did not change, a forced correction (e.g. after
            // bad_msg_notification(17) when local clock ran ahead before initial sync)
            // must allow msg_id generation to re-anchor to server time.
            // See tdesktop: Telegram/SourceFiles/mtproto/session_private.cpp:1636, 2175-2178.
            if force {
                guard.last_msg_id = 0;
            }
            return false;
        }
        guard.shift_secs = new_shift;
        guard.start_instant = Instant::now();
        guard.start_unix_secs = local + new_shift;
        // Reset last_msg_id whenever the server time shift changes (including initial DH
        // server_time sync in mtproto_dc_key_creator.cpp:665) so future msg_ids immediately
        // reflect the corrected server time instead of remaining pinned to an ahead local clock.
        guard.last_msg_id = 0;
        true
    }

    /// Updates HTTP-derived unix time from an HTTPS `Date` response header
    /// (`base::unixtime::http_update`).
    pub fn update_http_time(&self, http_unix_secs: i64) {
        let mut guard = self.inner.lock();
        guard.http_shift_secs = http_unix_secs - local_unix_secs();
        guard.http_updated = true;
    }

    /// Returns whether HTTP unix time has been synchronized (`base::unixtime::http_valid`).
    pub fn http_valid(&self) -> bool {
        self.inner.lock().http_updated
    }

    /// Current unix time preferring HTTP-synchronized time if available (`base::unixtime::http_now`).
    pub fn http_now_secs(&self) -> i32 {
        let guard = self.inner.lock();
        let shift = if guard.http_updated {
            guard.http_shift_secs
        } else {
            guard.shift_secs
        };
        (local_unix_secs() + shift) as i32
    }

    /// Generates the next client `msg_id`: time-based (`unix_secs << 32 | frac`), strictly
    /// monotonically increasing, and always divisible by 4 (`msg_id % 4 == 0`).
    pub fn next_msg_id(&self) -> u64 {
        let mut guard = self.inner.lock();
        let elapsed = guard.start_instant.elapsed();
        let secs = guard
            .start_unix_secs
            .saturating_add(elapsed.as_secs() as i64)
            .max(0) as u64;
        // Sub-second fractional part scaled to 2^32 (slightly slower than 1.0 like tdesktop's
        // 0xFFFF0000 multiplier so rapid calls don't drift into the future).
        let nanos = elapsed.subsec_nanos() as u64;
        let frac = ((nanos * 0xFFFF_0000u64) / 1_000_000_000u64) & !0x03u64;
        let mut candidate = (secs << 32) | frac;
        if candidate <= guard.last_msg_id {
            candidate = guard.last_msg_id.wrapping_add(4);
        }
        candidate &= !0x03u64;
        guard.last_msg_id = candidate;
        candidate
    }
}

/// Returns local system unix timestamp in seconds.
pub fn local_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Checks whether a server-originated `msg_id` has valid parity (`mod 4 == 1` or `mod 4 == 3`).
pub const fn is_valid_server_msg_id(msg_id: u64) -> bool {
    let rem = msg_id & 0x03;
    rem == 1 || rem == 3
}

/// Checks whether `server_time` is outside the acceptable window relative to `client_time`
/// (`serverTime > clientTime + 60 || serverTime + 300 < clientTime` from `session_private.cpp`).
pub const fn is_bad_server_time(server_time: i32, client_time: i32) -> bool {
    (server_time as i64) > (client_time as i64) + 60
        || (server_time as i64) + 300 < (client_time as i64)
}
