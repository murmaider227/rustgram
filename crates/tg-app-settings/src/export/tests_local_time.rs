//! The export date range reads and picks local time, like tdesktop's `base::unixtime::parse`.

use std::sync::{Mutex, MutexGuard, PoisonError};

use super::*;

static ZONE_LOCK: Mutex<()> = Mutex::new(());

/// Pins the local time zone to `offset_secs` east of UTC for one test. Every test of this
/// crate that reads local time holds one, so a pinned zone never leaks into another test.
pub(crate) struct ZoneGuard(#[allow(dead_code)] MutexGuard<'static, ()>);

impl ZoneGuard {
    pub(crate) fn east(offset_secs: i32) -> Self {
        let lock = ZONE_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        tg_history::datetime::set_utc_offset_override(Some(offset_secs));
        Self(lock)
    }
}

impl Drop for ZoneGuard {
    fn drop(&mut self) {
        tg_history::datetime::set_utc_offset_override(None);
    }
}

#[test]
fn labels_show_the_local_date_and_time() {
    let _zone = ZoneGuard::east(3 * 3600);
    // 2025-03-14 22:30 UTC is 2025-03-15 01:30 at UTC+3.
    let ts = civil_to_unix(2025, 3, 14, 22, 30, 0);
    assert_eq!(format_date_long(ts), "March 15, 2025");
    assert_eq!(format_time_short(ts), "01:30");
    assert_eq!(format_date_time_full(ts), "March 15, 2025 at 01:30");
    assert_eq!(
        TimePickerState::from_timestamp(ts),
        TimePickerState {
            hour: 1,
            minute: 30
        }
    );
}

#[test]
fn removing_the_time_keeps_local_midnight() {
    let _zone = ZoneGuard::east(3 * 3600);
    let ts = civil_to_unix(2025, 3, 14, 22, 30, 0);
    // Local midnight of 2025-03-15 at UTC+3 is 2025-03-14 21:00 UTC.
    assert_eq!(remove_time_of_day(ts), civil_to_unix(2025, 3, 14, 21, 0, 0));

    let mut settings = Settings {
        single_peer_from: ts,
        ..Settings::default()
    };
    apply_from_time(&mut settings, 9 * 3600);
    assert_eq!(
        settings.single_peer_from,
        civil_to_unix(2025, 3, 15, 6, 0, 0)
    );
}

#[test]
fn picked_day_is_local_midnight_and_reads_back_as_that_day() {
    let _zone = ZoneGuard::east(-5 * 3600);
    let picked = local_civil_to_unix(2025, 3, 15, 0, 0, 0);
    assert_eq!(picked, civil_to_unix(2025, 3, 15, 5, 0, 0));
    assert_eq!(format_date_long(picked), "March 15, 2025");
    assert_eq!(format_time_short(picked), "00:00");
    let state = DatePickerState::for_limit(picked, 0, 0);
    assert_eq!(state.highlighted, (2025, 3, 15));
}

#[test]
fn takeout_delay_names_the_local_time() {
    let _zone = ZoneGuard::east(2 * 3600);
    let now = civil_to_unix(2025, 3, 15, 10, 0, 0);
    let info = crate::export::manager::parse_takeout_init_delay("TAKEOUT_INIT_DELAY_7200", now)
        .expect("delay parses");
    // Available at 12:00 UTC = 14:00 local.
    assert!(
        info.message.contains("March 15, 2025 at 14:00"),
        "{}",
        info.message
    );
}
