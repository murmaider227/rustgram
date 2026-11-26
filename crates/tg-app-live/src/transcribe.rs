//! Live voice & round-video speech-to-text transcription glue (`messages.transcribeAudio`,
//! `messages.rateTranscribedAudio`, `updateTranscribedAudio`, and Premium/trial quota rules).
//!
//! Port origin:
//! - `Telegram/SourceFiles/api/api_transcribes.h` & `api_transcribes.cpp`
//! - `Telegram/SourceFiles/history/view/history_view_transcribe_button.cpp`
//! - `Telegram/SourceFiles/menu/menu_item_rate_transcribe_session.cpp`

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tg_api::RpcClient;
use tg_api::convert::{input_peer_for, peer_id_from_tl};
use tg_session::SessionRuntime;
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tokio::sync::broadcast;
use tracing::{debug, warn};

use crate::bridge::LiveRuntimeBridge;
use crate::convert::{from_data_peer, now_unix_i32, to_data_peer};
use crate::session::LiveSession;
use tg_app_core::history::backend::{
    TranscribeLimitsView, TranscribeToggleOutcome, TranscribeUiEntry,
};
use tg_app_core::stand_in::types::{MsgId, PeerId};

/// Default free-trial maximum audio duration in milliseconds (`300s = 5 min`,
/// matching `transcribe_audio_trial_duration_max` default `300` in `api_transcribes.cpp:101`).
pub const DEFAULT_TRIAL_MAX_DURATION_MS: i64 = 300_000;

/// Per-message transcription record (`Api::Transcribes::Entry`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranscribeEntry {
    /// Server-assigned `transcription_id` returned by `messages.transcribeAudio`.
    pub transcription_id: Option<i64>,
    /// Current recognized text (may be partial while `pending == true`).
    pub text: String,
    /// True if the transcript area is expanded in the chat bubble.
    pub shown: bool,
    /// True if the server returned `pending = true` and updates will arrive via
    /// `updateTranscribedAudio`.
    pub pending: bool,
    /// True if the RPC failed.
    pub failed: bool,
    /// True if the RPC failed with `MSG_VOICE_TOO_LONG`.
    pub too_long: bool,
    /// True if this transcription belongs to a round video message (`document->isVideoMessage()`).
    pub round_view: bool,
    /// True if the user already rated this transcription via `messages.rateTranscribedAudio`.
    pub rated: bool,
    /// True while a `messages.transcribeAudio` RPC is currently in flight.
    pub request_in_flight: bool,
}

impl TranscribeEntry {
    /// Converts this entry into the lightweight UI snapshot rendered by chat bubbles.
    #[must_use]
    pub fn to_ui_entry(&self) -> TranscribeUiEntry {
        TranscribeUiEntry {
            text: self.text.clone(),
            shown: self.shown,
            pending: self.pending || self.request_in_flight,
            failed: self.failed,
            too_long: self.too_long,
            round_view: self.round_view,
            can_rate: self.transcription_id.is_some()
                && !self.text.is_empty()
                && !self.pending
                && !self.request_in_flight
                && !self.failed
                && !self.rated,
            rated: self.rated,
        }
    }
}

/// Premium & free-trial quota state (`Api::Transcribes` trial accessors + `TranscribeButton::hasLock`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscribeLimits {
    /// Whether the current user has Telegram Premium (unlimited transcription).
    pub is_premium: bool,
    /// Whether weekly trial transcriptions are enabled by `appConfig`.
    pub trials_support: bool,
    /// Remaining free trial transcriptions in the current cooldown window (`trial_remains_num`).
    pub trials_count: i32,
    /// Unix timestamp when free trials reset (`trial_remains_until_date`).
    pub trials_refresh_at: i32,
    /// Maximum audio duration in milliseconds for free-trial transcriptions
    /// (`transcribe_audio_trial_duration_max * 1000`, default 300_000 ms).
    pub trials_max_length_ms: i64,
    /// Boost level required for a megagroup to grant free transcriptions (`groupFreeTranscribeLevel`).
    pub group_free_transcribe_level: i32,
}

impl Default for TranscribeLimits {
    fn default() -> Self {
        Self {
            is_premium: false,
            trials_support: true,
            trials_count: 2,
            trials_refresh_at: 0,
            trials_max_length_ms: DEFAULT_TRIAL_MAX_DURATION_MS,
            group_free_transcribe_level: 0,
        }
    }
}

impl TranscribeLimits {
    /// Mirrors `TranscribeButton::hasLock()` (`history_view_transcribe_button.cpp:332-349`):
    /// returns `true` when the button should display a Premium lock badge and prompt Premium.
    #[must_use]
    pub const fn has_lock(&self, now_unix: i32, free_for_peer: bool) -> bool {
        if self.is_premium || free_for_peer {
            return false;
        }
        if self.trials_count > 0 {
            return false;
        }
        if self.trials_refresh_at <= 0 || now_unix >= self.trials_refresh_at {
            return false;
        }
        true
    }

    /// Returns `true` when `duration_secs` exceeds `trials_max_length_ms` for a non-Premium user
    /// (`history_view_transcribe_button.cpp:399-411`).
    #[must_use]
    pub const fn exceeds_trial_duration(&self, duration_secs: u32, free_for_peer: bool) -> bool {
        if self.is_premium || free_for_peer {
            return false;
        }
        (duration_secs as i64) * 1000 > self.trials_max_length_ms
    }

    /// Converts to the UI view struct consumed by history bubbles.
    #[must_use]
    pub fn to_view(&self, now_unix: i32, free_for_peer: bool) -> TranscribeLimitsView {
        TranscribeLimitsView {
            is_premium: self.is_premium,
            locked: self.has_lock(now_unix, free_for_peer),
            trials_remaining: self.trials_count.max(0) as u32,
            trials_refresh_at: self.trials_refresh_at,
            trials_max_secs: (self.trials_max_length_ms / 1000).max(0) as u32,
        }
    }

    /// Formats the trial quota toast shown when `trial_remains_num` changes
    /// (`ShowTrialTranscribesToast` in `api_transcribes.cpp:206`).
    #[must_use]
    pub fn format_trial_toast(&self) -> String {
        if self.trials_count > 0 {
            format!(
                "{} free transcription{} remaining this week. Subscribe to Telegram Premium for unlimited voice-to-text.",
                self.trials_count,
                if self.trials_count == 1 { "" } else { "s" }
            )
        } else {
            "Weekly free transcriptions used up. Subscribe to Telegram Premium for unlimited voice-to-text."
                .to_owned()
        }
    }
}

/// Shared mutable transcription store owned by [`LiveSession`].
#[derive(Debug, Default)]
pub struct TranscribeState {
    /// Per-message transcription entries keyed by `(PeerId, MsgId)`.
    pub entries: HashMap<(PeerId, MsgId), TranscribeEntry>,
    /// Reverse index `transcription_id -> (PeerId, MsgId)` for `updateTranscribedAudio` routing.
    pub by_transcription_id: HashMap<i64, (PeerId, MsgId)>,
    /// Active Premium / free-trial limits.
    pub limits: TranscribeLimits,
    /// Peers known to qualify for free group transcriptions (`freeFor`).
    pub free_peers: std::collections::HashSet<PeerId>,
    /// Most recent trial-quota toast produced after a `messages.transcribeAudio` response.
    pub last_trial_toast: Option<String>,
}

impl TranscribeState {
    /// Creates a fresh transcription state with default limits.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies an incoming `updateTranscribedAudio` (`Api::Transcribes::apply`).
    /// Returns `true` if an existing or matching entry was updated.
    pub fn apply_update(&mut self, update: &tl_types::UpdateTranscribedAudio) -> bool {
        let key = self
            .by_transcription_id
            .get(&update.transcription_id)
            .copied()
            .unwrap_or_else(|| {
                let peer = from_data_peer(peer_id_from_tl(&update.peer));
                (peer, MsgId(update.msg_id))
            });

        let entry = self.entries.entry(key).or_default();
        entry.transcription_id = Some(update.transcription_id);
        entry.text = update.text.clone();
        entry.pending = update.pending;
        entry.request_in_flight = false;
        entry.failed = false;
        self.by_transcription_id
            .insert(update.transcription_id, key);
        true
    }
}

/// Spawns a background listener on `runtime.rpc().subscribe_updates()` that routes incoming
/// `tl_enums::Update::TranscribedAudio` events into `state` and triggers an egui repaint.
pub fn spawn_transcribe_update_listener<C: RpcClient + Clone + 'static>(
    runtime: &SessionRuntime<C>,
    bridge: &Arc<LiveRuntimeBridge>,
    state: &Arc<Mutex<TranscribeState>>,
) -> Option<tokio::task::JoinHandle<()>> {
    let mut rx = runtime.rpc().subscribe_updates()?;
    let state = Arc::clone(state);
    let bridge_state = Arc::clone(bridge.state());
    Some(runtime.tokio_handle().spawn(async move {
        loop {
            match rx.recv().await {
                Ok(updates) => {
                    if apply_updates_batch(&state, &updates) {
                        bridge_state.notify_ui();
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    debug!(skipped, "transcribe updates listener lagged");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }))
}

fn apply_updates_batch(state: &Arc<Mutex<TranscribeState>>, updates: &tl_enums::Updates) -> bool {
    let mut changed = false;
    let mut handle_one = |u: &tl_enums::Update| {
        if let tl_enums::Update::TranscribedAudio(tr) = u
            && let Ok(mut guard) = state.lock()
            && guard.apply_update(tr)
        {
            changed = true;
        }
    };
    match updates {
        tl_enums::Updates::UpdateShort(s) => handle_one(&s.update),
        tl_enums::Updates::Updates(u) => {
            for item in &u.updates {
                handle_one(item);
            }
        }
        tl_enums::Updates::Combined(c) => {
            for item in &c.updates {
                handle_one(item);
            }
        }
        _ => {}
    }
    changed
}

impl LiveSession {
    /// Updates Premium / trial transcription limits (e.g. from `help.getAppConfig` or tests).
    pub fn configure_transcribe_limits(&self, limits: TranscribeLimits) {
        if let Ok(mut guard) = self.transcribes.lock() {
            guard.limits = limits;
        }
    }

    /// Marks whether `peer_id` qualifies for free boosted-group transcription (`Transcribes::freeFor`).
    pub fn set_peer_free_transcribe(&self, peer_id: PeerId, free: bool) {
        if let Ok(mut guard) = self.transcribes.lock() {
            if free {
                guard.free_peers.insert(peer_id);
            } else {
                guard.free_peers.remove(&peer_id);
            }
        }
    }

    /// Returns a snapshot of the current transcription limits.
    #[must_use]
    pub fn transcribe_limits_snapshot(&self) -> TranscribeLimits {
        let self_is_premium =
            self.read_session(|s| s.user(s.self_user_id()).is_some_and(|u| u.is_premium()));
        let mut limits = self
            .transcribes
            .lock()
            .map(|g| g.limits.clone())
            .unwrap_or_default();
        if self_is_premium {
            limits.is_premium = true;
        }
        limits
    }

    /// Returns the UI view of limits for `peer_id` (`HistoryBackend::transcribe_limits`).
    #[must_use]
    pub fn transcribe_limits_view(&self, peer_id: Option<PeerId>) -> TranscribeLimitsView {
        let now = now_unix_i32();
        let free_for = peer_id.is_some_and(|p| {
            self.transcribes
                .lock()
                .is_ok_and(|g| g.free_peers.contains(&p))
        });
        self.transcribe_limits_snapshot().to_view(now, free_for)
    }

    /// Returns the UI transcription entry for `(peer_id, msg_id)`, if any.
    #[must_use]
    pub fn transcribe_entry_ui(&self, peer_id: PeerId, msg_id: MsgId) -> Option<TranscribeUiEntry> {
        self.transcribes.lock().ok().and_then(|g| {
            g.entries
                .get(&(peer_id, msg_id))
                .map(TranscribeEntry::to_ui_entry)
        })
    }

    /// Returns the full internal [`TranscribeEntry`] for `(peer_id, msg_id)`, if any.
    #[must_use]
    pub fn transcribe_entry(&self, peer_id: PeerId, msg_id: MsgId) -> Option<TranscribeEntry> {
        self.transcribes
            .lock()
            .ok()
            .and_then(|g| g.entries.get(&(peer_id, msg_id)).cloned())
    }

    /// Returns whether `(peer_id, msg_id)` has already been rated (`Transcribes::isRated`).
    #[must_use]
    pub fn is_transcription_rated(&self, peer_id: PeerId, msg_id: MsgId) -> bool {
        self.transcribes
            .lock()
            .ok()
            .and_then(|g| g.entries.get(&(peer_id, msg_id)).map(|e| e.rated))
            .unwrap_or(false)
    }

    /// Takes the most recent trial-quota toast message, if one was generated.
    pub fn take_transcribe_trial_toast(&self) -> Option<String> {
        self.transcribes
            .lock()
            .ok()
            .and_then(|mut g| g.last_trial_toast.take())
    }

    /// Directly applies a `tl_types::UpdateTranscribedAudio` update (`Transcribes::apply`).
    pub fn apply_transcribed_audio_update(
        &self,
        update: &tl_types::UpdateTranscribedAudio,
    ) -> bool {
        let updated = self
            .transcribes
            .lock()
            .is_ok_and(|mut g| g.apply_update(update));
        if updated {
            self.bridge_state().notify_ui();
        }
        updated
    }

    /// Toggles transcription for a voice or round-video message (`Api::Transcribes::toggle` +
    /// `TranscribeButton::link` click checks + `messages.transcribeAudio` RPC).
    pub fn toggle_transcribe_audio_live(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        duration_secs: u32,
        is_round_video: bool,
    ) -> TranscribeToggleOutcome {
        if msg_id.0 <= 0 {
            return TranscribeToggleOutcome::Failed(
                "Cannot transcribe a local/unsent message.".to_owned(),
            );
        }

        // 1. If we already have a non-in-flight entry, toggle visibility without re-requesting.
        {
            if let Ok(mut guard) = self.transcribes.lock()
                && let Some(existing) = guard.entries.get_mut(&(peer_id, msg_id))
            {
                if existing.request_in_flight {
                    return TranscribeToggleOutcome::Pending;
                }
                if !existing.failed {
                    existing.shown = !existing.shown;
                    let shown = existing.shown;
                    drop(guard);
                    self.bridge_state().notify_ui();
                    return TranscribeToggleOutcome::Toggled { shown };
                }
            }
        }

        // 2. Enforce Premium / trial lock & max duration checks (`history_view_transcribe_button.cpp`).
        let now = now_unix_i32();
        let limits = self.transcribe_limits_snapshot();
        let free_for = self
            .transcribes
            .lock()
            .is_ok_and(|g| g.free_peers.contains(&peer_id));

        if limits.has_lock(now, free_for) {
            return TranscribeToggleOutcome::PremiumRequired {
                refresh_at: limits.trials_refresh_at,
            };
        }
        if limits.exceeds_trial_duration(duration_secs, free_for) {
            return TranscribeToggleOutcome::TooLong {
                max_secs: (limits.trials_max_length_ms / 1000).max(0) as u32,
            };
        }

        // 3. Resolve TL `InputPeer` and mark request in flight.
        let data_peer = to_data_peer(peer_id);
        let input_peer = self.read_session(|s| input_peer_for(s, data_peer));
        if matches!(input_peer, tl_enums::InputPeer::Empty) {
            return TranscribeToggleOutcome::Failed(
                "Peer is not available for transcription.".to_owned(),
            );
        }

        if let Ok(mut guard) = self.transcribes.lock() {
            let entry = guard.entries.entry((peer_id, msg_id)).or_default();
            entry.shown = true;
            entry.failed = false;
            entry.too_long = false;
            entry.pending = false;
            entry.round_view = is_round_video;
            entry.request_in_flight = true;
        }

        // 4. Invoke `messages.transcribeAudio`.
        let req = tl_fns::messages::TranscribeAudio {
            peer: input_peer,
            msg_id: msg_id.0,
        };
        match self.invoke_rpc(req) {
            Ok(tl_enums::messages::TranscribedAudio::TranscribedAudio(res)) => {
                let mut trial_toast = None;
                if let Ok(mut guard) = self.transcribes.lock() {
                    let mut trials_changed = false;
                    if let Some(num) = res.trial_remains_num
                        && guard.limits.trials_count != num
                    {
                        guard.limits.trials_count = num;
                        guard.limits.trials_support = true;
                        trials_changed = true;
                    }
                    if let Some(until) = res.trial_remains_until_date
                        && guard.limits.trials_refresh_at != until
                    {
                        guard.limits.trials_refresh_at = until;
                        guard.limits.trials_support = true;
                    }
                    if trials_changed {
                        let msg = guard.limits.format_trial_toast();
                        guard.last_trial_toast = Some(msg.clone());
                        trial_toast = Some(msg);
                    }

                    let entry = guard.entries.entry((peer_id, msg_id)).or_default();
                    entry.request_in_flight = false;
                    entry.transcription_id = Some(res.transcription_id);
                    entry.pending = res.pending;
                    entry.text = res.text.clone();
                    entry.shown = true;
                    entry.failed = false;
                    entry.round_view = is_round_video;
                    guard
                        .by_transcription_id
                        .insert(res.transcription_id, (peer_id, msg_id));
                }
                self.bridge_state().notify_ui();
                if res.pending {
                    TranscribeToggleOutcome::Pending
                } else {
                    TranscribeToggleOutcome::Ready {
                        text: res.text,
                        trial_toast,
                    }
                }
            }
            Err(err) => {
                let err_str = err.to_string();
                let is_too_long = err_str.contains("MSG_VOICE_TOO_LONG");
                warn!(peer = peer_id.0, msg = msg_id.0, error = %err_str, "messages.transcribeAudio failed");
                if let Ok(mut guard) = self.transcribes.lock() {
                    let entry = guard.entries.entry((peer_id, msg_id)).or_default();
                    entry.request_in_flight = false;
                    entry.pending = false;
                    entry.failed = true;
                    entry.too_long = is_too_long;
                    entry.round_view = is_round_video;
                }
                self.bridge_state().notify_ui();
                // The bubble shows the failure (`entry.toolong` / `entry.failed`,
                // `api/api_transcribes.cpp:219-231`); `TooLong` is only the local length check.
                TranscribeToggleOutcome::Failed(err_str)
            }
        }
    }

    /// Rates a completed transcription (`messages.rateTranscribedAudio`, `Api::Transcribes::rate`).
    pub fn rate_transcribed_audio_live(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        good: bool,
    ) -> Result<(), String> {
        let transcription_id = self
            .transcribes
            .lock()
            .ok()
            .and_then(|g| g.entries.get(&(peer_id, msg_id))?.transcription_id)
            .ok_or_else(|| "No transcription available to rate for this message.".to_owned())?;

        let data_peer = to_data_peer(peer_id);
        let input_peer = self.read_session(|s| input_peer_for(s, data_peer));
        if matches!(input_peer, tl_enums::InputPeer::Empty) {
            return Err("Peer is not available.".to_owned());
        }

        let req = tl_fns::messages::RateTranscribedAudio {
            peer: input_peer,
            msg_id: msg_id.0,
            transcription_id,
            good,
        };
        self.invoke_rpc(req).map_err(|e| e.to_string())?;

        if let Ok(mut guard) = self.transcribes.lock()
            && let Some(entry) = guard.entries.get_mut(&(peer_id, msg_id))
        {
            entry.rated = true;
        }
        self.bridge_state().notify_ui();
        Ok(())
    }
}
