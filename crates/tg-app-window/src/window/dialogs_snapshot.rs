//! The whole chat list as `Backend::dialogs` returns it, kept between frames (TGB-2920).
//!
//! A separate chat window reads its chat's last message from this list every frame, and the
//! chat preview, Ctrl+Tab and "Open in new window" look one chat up in it. Converting the list
//! costs a conversion of every chat of the account, so it is read again only when
//! [`DialogsBackend::dialogs_version`] moves or the copy is older than
//! [`DIALOGS_SNAPSHOT_MAX_AGE`] (the demo's `Backend` list does not move that version, and
//! some fields follow the clock). Lookups by peer stay exactly what `dialogs()` returns,
//! archived chats and the demo's data set included.
//!
//! [`DialogsBackend::dialogs_version`]: tg_app_info::dialogs::backend::DialogsBackend::dialogs_version

use std::sync::Arc;
use std::time::{Duration, Instant};

use tg_app_core::stand_in::types::DialogEntry;

/// Longest time a copy is used without reading the list again.
pub const DIALOGS_SNAPSHOT_MAX_AGE: Duration = Duration::from_secs(1);

/// A shared copy of the chat list and what it was read at.
#[derive(Debug, Default)]
pub struct DialogsSnapshot {
    entries: Arc<Vec<DialogEntry>>,
    read: Option<(u64, Instant)>,
    reads: u64,
}

impl DialogsSnapshot {
    /// The list for `version` at `now`, read with `read` only when the copy is stale.
    pub fn get(
        &mut self,
        version: u64,
        now: Instant,
        read: impl FnOnce() -> Vec<DialogEntry>,
    ) -> Arc<Vec<DialogEntry>> {
        let fresh = self.read.is_some_and(|(v, at)| {
            v == version && now.saturating_duration_since(at) < DIALOGS_SNAPSHOT_MAX_AGE
        });
        if !fresh {
            self.entries = Arc::new(read());
            self.read = Some((version, now));
            self.reads += 1;
        }
        Arc::clone(&self.entries)
    }

    /// How many times the list was read (each read converts every chat).
    #[must_use]
    pub const fn reads(&self) -> u64 {
        self.reads
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_is_read_again_only_on_a_new_version_or_after_a_second() {
        let mut snap = DialogsSnapshot::default();
        let start = Instant::now();
        for frame in 0..60_u32 {
            let now = start + Duration::from_millis(u64::from(frame) * 16);
            let _ = snap.get(7, now, Vec::new);
        }
        assert_eq!(
            snap.reads(),
            1,
            "60 frames of one version read the list once"
        );
        let _ = snap.get(8, start + Duration::from_millis(990), Vec::new);
        assert_eq!(snap.reads(), 2, "a new version reads it again");
        let _ = snap.get(8, start + Duration::from_millis(1_995), Vec::new);
        assert_eq!(snap.reads(), 3, "and so does a copy older than a second");
    }
}
