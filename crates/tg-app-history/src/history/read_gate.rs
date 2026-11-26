//! Version gate for the backend reads an open chat repeats on every frame.
//!
//! `peer_info`, the pinned bar and the own name were read on each frame and returned the same
//! values as the frame before. [`BackendReadGate`] lets the view keep the last result while
//! [`HistoryBackend::chat_version`](crate::history::backend::HistoryBackend::chat_version)
//! stays the same.
//!
//! The version only covers what the backend counts as a change, so the gate also re-reads:
//! - once per wall-clock second (`peer_info` carries clock-derived text such as "last seen",
//!   and a missed version bump can then never show old data for longer than that),
//! - on a frame with input and on the frame after it (an action the user just took is read
//!   back at once, as before, whatever the backend does with its version).
//!
//! A backend that does not track changes returns a new version on every call, so its views
//! read on every frame exactly as they did without the gate.

use std::time::{SystemTime, UNIX_EPOCH};

use tg_app_core::stand_in::types::PeerId;

fn unix_second() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Decides once per frame whether the gated reads must be repeated.
#[derive(Debug, Default)]
pub(crate) struct BackendReadGate {
    /// `(peer, chat version, wall-clock second)` of the last frame.
    last: Option<(PeerId, u64, u64)>,
    input_last_frame: bool,
    /// Moves whenever the reads must be repeated; a [`Gated`] value read at an older generation
    /// is stale.
    generation: u64,
}

impl BackendReadGate {
    /// Call once per frame with the chat's version, read before the data it guards. Returns the
    /// generation to pass to [`Gated::get_or_read`].
    pub(crate) fn begin_frame(&mut self, ctx: &egui::Context, peer: PeerId, version: u64) -> u64 {
        let input_now = ctx.input(|i| !i.events.is_empty());
        let key = (peer, version, unix_second());
        if self.last != Some(key) || input_now || self.input_last_frame {
            self.generation = self.generation.wrapping_add(1);
        }
        self.last = Some(key);
        self.input_last_frame = input_now;
        self.generation
    }
}

/// A backend read kept while the gate's generation stays the same.
#[derive(Debug)]
pub(crate) struct Gated<T> {
    generation: u64,
    value: T,
}

impl<T> Gated<T> {
    /// The kept value, whatever generation it was read at.
    pub(crate) const fn value(&self) -> &T {
        &self.value
    }

    /// The kept value if it was read at `generation`, otherwise the result of `read`.
    pub(crate) fn get_or_read(
        slot: &mut Option<Self>,
        generation: u64,
        read: impl FnOnce() -> T,
    ) -> &T {
        let gated = match slot.take() {
            Some(kept) if kept.generation == generation => kept,
            _ => Self {
                generation,
                value: read(),
            },
        };
        &slot.insert(gated).value
    }
}
