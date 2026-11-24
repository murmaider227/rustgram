//! Pure fact-check DTOs and request batcher (`history/factcheck.rs` model).

use std::collections::{HashMap, HashSet, VecDeque};

use tg_text::TextWithEntities;

use crate::history::backend::HistoryMedia;
use crate::stand_in::types::{FullMsgId, MsgId, PeerId};

/// Delay before flushing pending `messages.getFactCheck` requests (`kRequestDelay = 1000ms`).
pub const FACTCHECK_REQUEST_DELAY_MS: u64 = 1_000;

/// Default maximum character length of a fact-check note (`factcheck_length_limit = 1024`).
pub const FACTCHECK_DEFAULT_LENGTH_LIMIT: usize = 1_024;

/// Maximum preview character count shown in the collapsed bubble footer before expanding.
pub const FACTCHECK_COLLAPSED_PREVIEW_CHARS: usize = 96;

/// Fact-check note attached to a channel post (`FactCheck` TL constructor).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FactCheckData {
    /// True if the server indicates the full note must be fetched via `messages.getFactCheck`.
    pub need_check: bool,
    /// Optional ISO-3166 alpha-2 country code (e.g. `"US"`, `"DE"`).
    pub country: Option<String>,
    /// Fact-check note text with formatting entities.
    pub text: Option<TextWithEntities>,
    /// Server hash for caching/deduplication.
    pub hash: i64,
}

impl FactCheckData {
    /// Returns `true` if this fact-check has no visible text and does not need a fetch.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.need_check && self.text.as_ref().is_none_or(|t| t.text.trim().is_empty())
    }

    /// Returns the non-empty plain text body, if resolved.
    #[must_use]
    pub fn body_text(&self) -> Option<&str> {
        self.text
            .as_ref()
            .map(|t| t.text.trim())
            .filter(|s| !s.is_empty())
    }

    /// Returns the formatted header label (`"Fact-Check"` or `"Fact-Check (XX)"`).
    #[must_use]
    pub fn header_label(&self) -> String {
        match self.country.as_deref().filter(|c| !c.is_empty()) {
            Some(cc) => format!("Fact-Check ({})", cc.to_uppercase()),
            None => "Fact-Check".to_owned(),
        }
    }

    /// Returns `true` when the note is long enough to offer a Show more / Show less toggle.
    #[must_use]
    pub fn is_expandable(&self) -> bool {
        self.text
            .as_ref()
            .is_some_and(|t| t.text.chars().count() > FACTCHECK_COLLAPSED_PREVIEW_CHARS)
    }
}

/// Pure-logic permission check matching `Data::Factchecks::canEdit` (`factchecks.cpp:141-154`).
#[must_use]
pub fn can_edit_factcheck(
    app_config_can_edit: bool,
    is_broadcast: bool,
    is_regular: bool,
    media: Option<&HistoryMedia>,
) -> bool {
    if !app_config_can_edit || !is_broadcast || !is_regular {
        return false;
    }
    match media {
        None
        | Some(HistoryMedia::Photo(_))
        | Some(HistoryMedia::Album(_))
        | Some(HistoryMedia::Video(_))
        | Some(HistoryMedia::Document(_))
        | Some(HistoryMedia::Voice(_)) => true,
        Some(
            HistoryMedia::Sticker(_)
            | HistoryMedia::RoundVideo(_)
            | HistoryMedia::Poll(_)
            | HistoryMedia::Location(_)
            | HistoryMedia::Contact(_)
            | HistoryMedia::Game(_)
            | HistoryMedia::TodoList(_)
            | HistoryMedia::Widget(_)
            | HistoryMedia::Story(_)
            | HistoryMedia::Giveaway(_)
            | HistoryMedia::GiveawayResults(_)
            | HistoryMedia::VideoStream(_),
        ) => false,
    }
}

/// Batches `messages.getFactCheck` requests per peer with a 1-second coalesce timer and
/// immediate flush when switching to a different peer (`Data::Factchecks` in `factchecks.cpp`).
#[derive(Debug, Default, Clone)]
pub struct FactCheckBatcher {
    pending: VecDeque<FullMsgId>,
    pending_set: HashSet<FullMsgId>,
    in_flight: Vec<Option<FullMsgId>>,
    in_flight_peer: Option<PeerId>,
    first_enqueued_ms: Option<u64>,
    requested_ever: HashSet<FullMsgId>,
}

impl FactCheckBatcher {
    /// Creates an empty fact-check batcher.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Enqueues `item_id` for a batched `messages.getFactCheck` call.
    ///
    /// Returns `true` if switching peers triggered an immediate flush recommendation
    /// (`if (changed) request();` in `factchecks.cpp:46-50`).
    pub fn enqueue(&mut self, item_id: FullMsgId, now_ms: u64) -> bool {
        if self.requested_ever.contains(&item_id) || !self.pending_set.insert(item_id) {
            return false;
        }
        let peer_changed = self
            .pending
            .front()
            .is_some_and(|first| first.peer != item_id.peer);
        self.pending.push_back(item_id);
        self.requested_ever.insert(item_id);
        if self.first_enqueued_ms.is_none() {
            self.first_enqueued_ms = Some(now_ms);
        }
        peer_changed
    }

    /// Removes a deleted item from pending and in-flight queues (`factchecks.cpp:62-69`).
    pub fn on_item_removed(&mut self, item_id: FullMsgId) {
        self.pending_set.remove(&item_id);
        self.pending.retain(|id| *id != item_id);
        for slot in &mut self.in_flight {
            if *slot == Some(item_id) {
                *slot = None;
            }
        }
        if self.pending.is_empty() {
            self.first_enqueued_ms = None;
        }
    }

    /// Takes the next ready single-peer batch `(peer_id, msg_ids)` if no request is currently
    /// in flight and either `force` is `true`, a second peer is waiting, or the 1s delay elapsed.
    pub fn take_ready_batch(&mut self, now_ms: u64, force: bool) -> Option<(PeerId, Vec<MsgId>)> {
        if !self.in_flight.is_empty() || self.pending.is_empty() {
            return None;
        }
        let front_peer = self.pending.front()?.peer;
        let has_other_peer = self.pending.iter().any(|id| id.peer != front_peer);
        let delay_elapsed = self
            .first_enqueued_ms
            .is_some_and(|t| now_ms >= t.saturating_add(FACTCHECK_REQUEST_DELAY_MS));
        if !force && !has_other_peer && !delay_elapsed {
            return None;
        }

        let mut remaining = VecDeque::new();
        let mut msg_ids = Vec::new();
        while let Some(item) = self.pending.pop_front() {
            if item.peer == front_peer {
                self.pending_set.remove(&item);
                self.in_flight.push(Some(item));
                msg_ids.push(item.msg);
            } else {
                remaining.push_back(item);
            }
        }
        self.pending = remaining;
        self.first_enqueued_ms = (!self.pending.is_empty()).then_some(now_ms);
        self.in_flight_peer = Some(front_peer);
        Some((front_peer, msg_ids))
    }

    /// Finishes the active in-flight batch and pairs each non-removed message id with its
    /// returned [`FactCheckData`] (or empty [`FactCheckData::default`] on short/failed response).
    pub fn finish_batch(
        &mut self,
        results: Result<Vec<FactCheckData>, ()>,
    ) -> HashMap<FullMsgId, FactCheckData> {
        let items = std::mem::take(&mut self.in_flight);
        self.in_flight_peer = None;
        let mut resolved = HashMap::new();
        match results {
            Ok(list) => {
                for (idx, maybe_id) in items.into_iter().enumerate() {
                    if let Some(full_id) = maybe_id {
                        let fc = list.get(idx).cloned().unwrap_or_default();
                        resolved.insert(full_id, fc);
                    }
                }
            }
            Err(()) => {
                for maybe_id in items.into_iter().flatten() {
                    resolved.insert(maybe_id, FactCheckData::default());
                }
            }
        }
        resolved
    }

    /// Returns `true` if there are queued or in-flight fact-check requests.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty() || !self.in_flight.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Fact-check bubble footer with expandable note
// ---------------------------------------------------------------------------
