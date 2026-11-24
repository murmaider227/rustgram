//! Live RPC integration for Dialogs promo suggestions (`help.getPromoData` /
//! `help.dismissSuggestion`).
//!
//! Port of `Telegram/SourceFiles/data/components/promo_suggestions.cpp`.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tg_api::RpcClient;
use tg_tl::{enums as tl_enums, functions as tl_fns};

use crate::media::with_rpc_timeout;
use crate::session::LiveSession;
use tg_app_core::core_types::misc::{
    PromoDataSnapshot, PromoSuggestion, PromoSuggestionKind, SuggestionPriority,
};
use tg_app_core::stand_in::types::PeerId;

/// Timeout for background promo-suggestion RPCs.
const PROMO_RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// Delay before retrying `help.getPromoData` after a failed request.
const PROMO_RETRY_DELAY_SECS: i32 = 60;

/// Cached server promo-suggestions state stored per [`LiveSession`].
#[derive(Debug, Default, Clone)]
pub struct LivePromoState {
    loaded: bool,
    in_flight: bool,
    retry_at: i32,
    snapshot: Option<PromoDataSnapshot>,
    dismissed_keys: BTreeSet<String>,
}

impl LivePromoState {
    /// Creates an unloaded [`LivePromoState`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Forces the next [`load_promo_data`] call to re-fetch `help.getPromoData`.
    pub fn invalidate(&mut self) {
        self.loaded = false;
        self.retry_at = 0;
    }

    fn needs_fetch(&self, now_unix: i32) -> bool {
        if self.in_flight {
            return false;
        }
        if !self.loaded {
            return true;
        }
        match &self.snapshot {
            Some(snap) => snap.expires > 0 && now_unix >= snap.expires,
            None => now_unix >= self.retry_at,
        }
    }

    fn apply_fetched(&mut self, mut snap: PromoDataSnapshot) {
        for key in &self.dismissed_keys {
            hide_key(&mut snap, key);
        }
        self.snapshot = Some(snap);
    }

    fn apply_dismissed_locally(&mut self, key: &str) {
        self.dismissed_keys.insert(key.to_owned());
        if let Some(snap) = self.snapshot.as_mut() {
            hide_key(snap, key);
        }
    }

    /// Clears a hidden promoted / PSA peer from the cached snapshot (`help.hidePromoData`,
    /// `window/window_peer_menu.cpp:525-530`).
    pub(crate) fn hide_promo_peer(&mut self, peer_id: PeerId) {
        if let Some(snap) = self.snapshot.as_mut()
            && snap.promo_peer == Some(peer_id)
        {
            snap.promo_peer = None;
            snap.psa_type = None;
            snap.psa_message = None;
        }
    }

    #[cfg(test)]
    pub(crate) fn seed_snapshot_for_test(&mut self, snap: PromoDataSnapshot) {
        self.loaded = true;
        self.in_flight = false;
        self.snapshot = Some(snap);
    }

    #[cfg(test)]
    pub(crate) fn snapshot_for_test(&self) -> Option<PromoDataSnapshot> {
        self.snapshot.clone()
    }
}

fn hide_key(snap: &mut PromoDataSnapshot, key: &str) {
    if !snap.dismissed_suggestions.iter().any(|k| k == key) {
        snap.dismissed_suggestions.push(key.to_owned());
    }
    snap.pending_suggestions.retain(|k| k != key);
    if snap
        .custom_suggestion
        .as_ref()
        .is_some_and(|c| c.key == key)
    {
        snap.custom_suggestion = None;
    }
}

/// Returns the cached promo suggestions for `live` and, when the cache is empty or expired,
/// starts a background `help.getPromoData` request that repaints the UI when it lands
/// (`Data::PromoSuggestions::refresh`).
pub fn load_promo_data(
    live: &LiveSession,
    state: &Arc<Mutex<LivePromoState>>,
    now_unix: i32,
) -> Option<PromoDataSnapshot> {
    let mut guard = state.lock().ok()?;
    if guard.needs_fetch(now_unix) {
        guard.in_flight = true;
        let rpc = live.runtime().rpc().clone();
        let bridge = Arc::clone(live.bridge_state());
        let state = Arc::clone(state);
        live.runtime().tokio_handle().spawn(async move {
            let fetched =
                with_rpc_timeout(PROMO_RPC_TIMEOUT, async { fetch_promo_data(&rpc).await }).await;
            if let Ok(mut guard) = state.lock() {
                guard.in_flight = false;
                guard.loaded = true;
                match fetched {
                    Ok(snap) => guard.apply_fetched(snap),
                    Err(err) => {
                        guard.retry_at = now_unix.saturating_add(PROMO_RETRY_DELAY_SECS);
                        tracing::warn!(error = %err, "help.getPromoData failed");
                    }
                }
            }
            bridge.notify_ui();
        });
    }
    guard.snapshot.clone()
}

/// Hides `suggestion` locally at once and sends `help.dismissSuggestion` in the background
/// (`Data::PromoSuggestions::dismiss`, which does not wait for the reply either).
pub fn dismiss_promo_suggestion_async(
    live: &LiveSession,
    state: &Mutex<LivePromoState>,
    peer_id: Option<PeerId>,
    suggestion: &str,
) {
    if let Ok(mut guard) = state.lock() {
        guard.apply_dismissed_locally(suggestion);
    }
    let input_peer = peer_id
        .and_then(|pid| live.resolve_input_peer(pid))
        .unwrap_or_else(empty_input_peer);
    let rpc = live.runtime().rpc().clone();
    let key = suggestion.to_owned();
    live.runtime().tokio_handle().spawn(async move {
        if let Err(err) = with_rpc_timeout(PROMO_RPC_TIMEOUT, async {
            dismiss_promo_suggestion(&rpc, input_peer, key).await
        })
        .await
        {
            tracing::warn!(error = %err, "help.dismissSuggestion failed");
        }
    });
    live.bridge_state().notify_ui();
}

/// Converts a TL `help.PromoData` response into a [`PromoDataSnapshot`].
#[must_use]
pub fn promo_data_from_tl(promo: &tl_enums::help::PromoData) -> PromoDataSnapshot {
    match promo {
        tl_enums::help::PromoData::Empty(empty) => PromoDataSnapshot {
            expires: empty.expires,
            ..PromoDataSnapshot::default()
        },
        tl_enums::help::PromoData::PromoData(data) => {
            let promo_peer = data.peer.as_ref().map(peer_id_from_tl);
            let custom_suggestion = data.custom_pending_suggestion.as_ref().map(|custom| {
                let tl_enums::PendingSuggestion::PendingSuggestion(ps) = custom;
                let tl_enums::TextWithEntities::TextWithEntities(title) = &ps.title;
                let tl_enums::TextWithEntities::TextWithEntities(desc) = &ps.description;
                PromoSuggestion {
                    key: ps.suggestion.clone(),
                    priority: SuggestionPriority::CustomPromo,
                    kind: PromoSuggestionKind::CustomPromo {
                        url: ps.url.clone(),
                    },
                    title: title.text.clone(),
                    description: desc.text.clone(),
                    peer_id: promo_peer,
                    dismissible: true,
                }
            });

            PromoDataSnapshot {
                expires: data.expires,
                pending_suggestions: data.pending_suggestions.clone(),
                dismissed_suggestions: data.dismissed_suggestions.clone(),
                custom_suggestion,
                promo_peer,
                psa_type: data.psa_type.clone(),
                psa_message: data.psa_message.clone(),
            }
        }
    }
}

fn peer_id_from_tl(peer: &tl_enums::Peer) -> PeerId {
    match peer {
        tl_enums::Peer::User(u) => PeerId::from_user(u.user_id.max(0) as u64),
        tl_enums::Peer::Chat(c) => PeerId::from_chat(c.chat_id.max(0) as u64),
        tl_enums::Peer::Channel(ch) => PeerId::from_channel(ch.channel_id.max(0) as u64),
    }
}

/// Fetches top-bar promo suggestions via `help.getPromoData`.
pub async fn fetch_promo_data<C>(client: &C) -> Result<PromoDataSnapshot, String>
where
    C: RpcClient,
{
    let resp = client
        .invoke(&tl_fns::help::GetPromoData {})
        .await
        .map_err(|e| {
            let inv: tg_api::InvocationError = e.into();
            format!("help.getPromoData failed: {inv}")
        })?;
    Ok(promo_data_from_tl(&resp))
}

/// Dismisses a server promo suggestion via `help.dismissSuggestion`.
pub async fn dismiss_promo_suggestion<C>(
    client: &C,
    peer: tl_enums::InputPeer,
    suggestion: impl Into<String>,
) -> Result<bool, String>
where
    C: RpcClient,
{
    client
        .invoke(&tl_fns::help::DismissSuggestion {
            peer,
            suggestion: suggestion.into(),
        })
        .await
        .map_err(|e| {
            let inv: tg_api::InvocationError = e.into();
            format!("help.dismissSuggestion failed: {inv}")
        })
}

/// Builds a default `InputPeer::Empty` for global suggestion dismissal.
#[must_use]
pub fn empty_input_peer() -> tl_enums::InputPeer {
    tl_enums::InputPeer::Empty
}
