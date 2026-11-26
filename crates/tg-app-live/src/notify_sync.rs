//! Keeps the notification system's mute view of a live account current (TGB-2969).
//!
//! tdesktop reads `Data::NotifySettings` directly when it decides whether to notify
//! (`Window::Notifications::System::computeSkipState`). Rustgram's notification system keeps its
//! own [`NotifySettingsStore`] per account instead, so this module provides:
//! - a version that moves whenever any notification setting of the account changes (dialog
//!   pages, `updateNotifySettings`, per-type defaults, local edits), so the store is re-synced
//!   right before queued notifications are evaluated;
//! - the lazy load of the three per-type defaults (`account.getNotifySettings` for users, groups
//!   and channels), which tdesktop requests from `NotifySettings::request(peer)` the first time a
//!   notification needs them. Until they arrive, notifications wait (`SkipState::Unknown`);
//! - the load of one peer's own settings (`account.getNotifySettings(inputNotifyPeer)`) for the
//!   author of a message that mentions the user in a group or channel, when those settings were
//!   never received (TGB-2980). `System::computeSkipState` calls `NotifySettings::request(notifyBy)`
//!   and answers `SkipState::Unknown` for a muted chat until they arrive; here the author is
//!   marked unknown in the store for as long as the request runs. A request that fails leaves
//!   the author on the per-type default, so a notification is never held for ever.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tg_api::RpcClient;
use tg_api::account::default_notify_scope_to_tl;
use tg_api::convert::peer_notify_settings_from_tl;
use tg_app_core::notifications::settings::NotifySettingsStore;
use tg_data::{DefaultNotifyScope, Message, PeerId as DataPeerId, PeerNotifySettings, Session};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tracing::debug;

use crate::bridge::LiveBridgeState;
use crate::convert::now_unix_i32;
use crate::session::LiveSession;

/// Time limit of one `account.getNotifySettings` request.
const DEFAULTS_RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// Delay before a failed defaults load is tried again.
const DEFAULTS_RETRY_DELAY: Duration = Duration::from_secs(30);

/// State of the per-type defaults load.
#[derive(Debug, Default)]
pub(crate) struct NotifyDefaultsLoad {
    in_flight: bool,
    loaded: bool,
    retry_at: Option<Instant>,
}

impl NotifyDefaultsLoad {
    /// Claims the next load attempt, if one is due.
    fn try_start(&mut self, now: Instant) -> bool {
        if self.loaded || self.in_flight || self.retry_at.is_some_and(|at| now < at) {
            return false;
        }
        self.in_flight = true;
        true
    }

    fn finish(&mut self, all_loaded: bool, now: Instant) {
        self.in_flight = false;
        self.loaded = all_loaded;
        self.retry_at = (!all_loaded).then(|| now + DEFAULTS_RETRY_DELAY);
    }
}

/// Version of every notification setting of the account: local edits (the session cache
/// counter) plus server-side changes seen on the change stream (the bridge counter). Both only
/// grow, so their sum moves whenever either does.
pub(crate) fn notify_settings_epoch(live: &LiveSession) -> u64 {
    let local = live
        .cache
        .lock()
        .map(|c| c.notify_settings_version)
        .unwrap_or(0);
    local.wrapping_add(live.bridge_state().notify_settings_generation())
}

/// Starts loading the per-type defaults in the background once the session is ready, unless
/// they are loaded, loading, or waiting for a retry (`NotifySettings::request(DefaultNotify)`).
pub(crate) fn request_notify_defaults(live: &LiveSession) {
    if !live.runtime().is_ready() {
        return;
    }
    let start = live
        .cache
        .lock()
        .is_ok_and(|mut c| c.notify_defaults_load.try_start(Instant::now()));
    if !start {
        return;
    }
    let rpc = live.runtime().rpc().clone();
    let session = Arc::clone(live.runtime().session());
    let cache = Arc::clone(&live.cache);
    let bridge = Arc::clone(live.bridge_state());
    live.runtime().tokio_handle().spawn(async move {
        let mut all_loaded = true;
        for scope in DefaultNotifyScope::ALL {
            if session.read().await.notify_default(scope).is_some() {
                continue;
            }
            let req = tl_fns::account::GetNotifySettings {
                peer: default_notify_scope_to_tl(scope),
            };
            match tokio::time::timeout(DEFAULTS_RPC_TIMEOUT, rpc.invoke(&req)).await {
                Ok(Ok(settings)) => {
                    let data = peer_notify_settings_from_tl(&settings);
                    session
                        .write()
                        .await
                        .set_notify_default(scope, Some(data), now_unix_i32());
                }
                Ok(Err(err)) => {
                    debug!(?scope, error = %err, "account.getNotifySettings (default) failed");
                    all_loaded = false;
                }
                Err(_) => {
                    debug!(?scope, "account.getNotifySettings (default) timed out");
                    all_loaded = false;
                }
            }
        }
        if let Ok(mut c) = cache.lock() {
            c.notify_defaults_load.finish(all_loaded, Instant::now());
        }
        bridge.notify_ui();
    });
}

/// Peers whose own notification settings are being loaded for a waiting notification.
#[derive(Debug, Default)]
pub(crate) struct PeerSettingsRequests {
    /// Asked for by the change stream, not requested yet.
    wanted: BTreeSet<DataPeerId>,
    in_flight: BTreeSet<DataPeerId>,
    /// Requests that are over, answered or failed. The store marked these peers unknown while
    /// it waited, so it must be given their settings again even when they are the defaults.
    finished: BTreeSet<DataPeerId>,
}

impl PeerSettingsRequests {
    /// Notes that `peer`'s settings are needed. `false` when they are already asked for.
    fn want(&mut self, peer: DataPeerId) -> bool {
        !self.in_flight.contains(&peer) && self.wanted.insert(peer)
    }

    fn start_wanted(&mut self) -> Vec<DataPeerId> {
        let wanted = std::mem::take(&mut self.wanted);
        self.in_flight.extend(wanted.iter().copied());
        wanted.into_iter().collect()
    }

    fn finish(&mut self, peer: DataPeerId) {
        self.in_flight.remove(&peer);
        self.finished.insert(peer);
    }

    /// Peers whose request is over; see the field.
    pub(crate) fn finished(&self) -> impl Iterator<Item = DataPeerId> + '_ {
        self.finished.iter().copied()
    }

    /// Peers whose settings are not here yet: the notification system must wait for them.
    pub(crate) fn awaiting(&self) -> impl Iterator<Item = DataPeerId> + '_ {
        self.wanted.iter().chain(&self.in_flight).copied()
    }
}

/// Called for a new message that may notify: asks for the settings of its special notification
/// peer (`HistoryItem::specialNotificationPeer`, the author of a mention outside private chats)
/// when they were never received (`NotifySettings::request(notifyBy)`).
pub(crate) fn want_mention_author_settings(
    session: &Session,
    msg: &Message,
    state: &LiveBridgeState,
) {
    if !msg.mentions_me() || msg.peer_id.is_user() {
        return;
    }
    let Some(author) = msg.from_id else {
        return;
    };
    if author == msg.peer_id || session.peer(author).is_none() || session.peer_notify_known(author)
    {
        return;
    }
    let added = state
        .notify_peer_requests
        .lock()
        .is_ok_and(|mut requests| requests.want(author));
    if added {
        // The store marks the author unknown before the message is judged.
        state.bump_notify_settings();
    }
}

/// Sends `account.getNotifySettings` for every peer asked for since the last call.
pub(crate) fn request_wanted_peer_settings(live: &LiveSession) {
    if !live.runtime().is_ready() {
        return;
    }
    let wanted = live
        .bridge_state()
        .notify_peer_requests
        .lock()
        .map(|mut requests| requests.start_wanted())
        .unwrap_or_default();
    if wanted.is_empty() {
        return;
    }
    let rpc = live.runtime().rpc().clone();
    let session = Arc::clone(live.runtime().session());
    let bridge = Arc::clone(live.bridge_state());
    live.runtime().tokio_handle().spawn(async move {
        for peer in wanted {
            let input = tg_api::convert::input_peer_for(&*session.read().await, peer);
            let req = tl_fns::account::GetNotifySettings {
                peer: tl_enums::InputNotifyPeer::InputNotifyPeer(tl_types::InputNotifyPeer {
                    peer: input,
                }),
            };
            match tokio::time::timeout(DEFAULTS_RPC_TIMEOUT, rpc.invoke(&req)).await {
                Ok(Ok(settings)) => {
                    let data = peer_notify_settings_from_tl(&settings);
                    session
                        .write()
                        .await
                        .set_peer_notify(peer, data, now_unix_i32());
                }
                Ok(Err(err)) => {
                    debug!(error = %err, "account.getNotifySettings (peer) failed");
                }
                Err(_) => debug!("account.getNotifySettings (peer) timed out"),
            }
            if let Ok(mut requests) = bridge.notify_peer_requests.lock() {
                requests.finish(peer);
            }
            // Settings equal to what was stored announce nothing; the waiting notification
            // must still be judged again.
            bridge.bump_notify_settings();
            bridge.notify_ui();
        }
    });
}

/// Applies `account.resetNotifySettings` locally: every per-type default back to "notify with
/// the default sound and previews", every chat and author exception removed.
pub(crate) fn reset_session_notify_settings(session: &mut Session) {
    let now = now_unix_i32();
    for scope in DefaultNotifyScope::ALL {
        let unmuted = PeerNotifySettings {
            mute_until: Some(0),
            show_previews: Some(true),
            sound: Some("default".to_owned()),
            ..PeerNotifySettings::default()
        };
        session.set_notify_default(scope, Some(unmuted), now);
    }
    let overridden: Vec<_> = session.peers_with_notify_settings().collect();
    for peer_id in overridden {
        session.set_peer_notify(peer_id, PeerNotifySettings::default(), now);
    }
}

/// Syncs `store` with the live account and makes sure the per-type defaults get loaded.
pub(crate) fn sync_store(live: &LiveSession, store: &mut NotifySettingsStore, now_unix: i64) {
    request_notify_defaults(live);
    crate::settings_sync::sync_notify_settings_store_live(live, store, now_unix);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_settings_are_wanted_once_and_awaited_until_the_request_finishes() {
        let peer = DataPeerId::from_user(tg_data::UserId(7));
        let mut requests = PeerSettingsRequests::default();
        assert!(requests.want(peer));
        assert!(!requests.want(peer), "asked for once");
        assert_eq!(requests.awaiting().collect::<Vec<_>>(), [peer]);
        assert_eq!(requests.start_wanted(), [peer]);
        assert!(requests.start_wanted().is_empty());
        assert!(!requests.want(peer), "not asked again while in flight");
        assert_eq!(requests.awaiting().collect::<Vec<_>>(), [peer]);
        requests.finish(peer);
        assert_eq!(requests.awaiting().count(), 0);
        assert_eq!(requests.finished().collect::<Vec<_>>(), [peer]);
    }

    #[test]
    fn defaults_load_retries_after_failure_and_stops_once_loaded() {
        let t0 = Instant::now();
        let mut load = NotifyDefaultsLoad::default();
        assert!(load.try_start(t0));
        assert!(!load.try_start(t0), "one request at a time");
        load.finish(false, t0);
        assert!(!load.try_start(t0 + Duration::from_secs(1)));
        assert!(load.try_start(t0 + DEFAULTS_RETRY_DELAY));
        load.finish(true, t0 + DEFAULTS_RETRY_DELAY);
        assert!(!load.try_start(t0 + DEFAULTS_RETRY_DELAY * 10));
    }
}
