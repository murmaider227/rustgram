//! Per-frame UI reads that never wait for the session lock once they have an answer
//! (TGB-2932, first step).
//!
//! The chat header (`HistoryBackend::peer_info`) and the chat list page
//! (`DialogsBackend::dialog_slice`) are read from the session whenever their version moves. When
//! the session actor holds the write lock at that moment (applying an update batch), the UI used
//! to block until it was released. Now such a read returns the previous answer for the same
//! arguments at once and moves the view versions again, so the next frame reads the new state
//! (by then the write is over): what the user sees is at most one frame older, and nothing waits.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tg_app_core::dialogs::backend::{DialogListQuery, DialogSlice};
use tg_app_core::history::backend::HistoryPeerInfo;
use tg_app_core::stand_in::types::PeerId;

/// How many chat headers / list pages are remembered (the open chat and a few recent ones).
const KEEP: usize = 8;

/// Last answers of the reads above, shared by every clone of a `LiveSession`.
#[derive(Clone, Default)]
pub(crate) struct UiReadCache {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    peer_info: HashMap<PeerId, Option<HistoryPeerInfo>>,
    dialog_slices: HashMap<(DialogListQuery, usize, usize), DialogSlice>,
    /// Reads answered from here because the lock was busy (tests, diagnostics).
    served_last: u64,
}

fn remember<K: std::hash::Hash + Eq + Copy, V>(map: &mut HashMap<K, V>, key: K, value: V) {
    if map.len() >= KEEP && !map.contains_key(&key) {
        map.clear();
    }
    map.insert(key, value);
}

impl UiReadCache {
    pub(crate) fn last_peer_info(&self, peer: PeerId) -> Option<Option<HistoryPeerInfo>> {
        let mut inner = self.inner.lock().ok()?;
        let last = inner.peer_info.get(&peer).cloned()?;
        inner.served_last += 1;
        Some(last)
    }

    pub(crate) fn store_peer_info(&self, peer: PeerId, info: &Option<HistoryPeerInfo>) {
        if let Ok(mut inner) = self.inner.lock() {
            remember(&mut inner.peer_info, peer, info.clone());
        }
    }

    pub(crate) fn last_dialog_slice(
        &self,
        key: (DialogListQuery, usize, usize),
    ) -> Option<DialogSlice> {
        let mut inner = self.inner.lock().ok()?;
        let last = inner.dialog_slices.get(&key).cloned()?;
        inner.served_last += 1;
        Some(last)
    }

    pub(crate) fn store_dialog_slice(
        &self,
        key: (DialogListQuery, usize, usize),
        slice: &DialogSlice,
    ) {
        if let Ok(mut inner) = self.inner.lock() {
            remember(&mut inner.dialog_slices, key, slice.clone());
        }
    }

    /// Reads answered from the cache so far.
    #[cfg(test)]
    pub(crate) fn served_last(&self) -> u64 {
        self.inner.lock().map_or(0, |i| i.served_last)
    }
}
