//! Per-type default notification settings and effective mute state
//! (`Data::NotifySettings::_defaultValues`, `NotifySettings::isMuted(peer)` and
//! `NotifySettings::updateLocal(DefaultNotify)` in `data/notify/data_notify_settings.cpp`).
//!
//! The defaults live next to the peers they apply to so every reader (chat list mute icons,
//! unread badges, the notification system) resolves "muted" the same way, and every change
//! reaches them through the session change stream.

use super::Session;
use crate::changes::{ChangeEvent, PeerUpdateFlags};
use crate::id::{PeerId, TimeId};
use crate::peer::{Peer, PeerNotifySettings};

/// Default notification scope (`Data::DefaultNotify`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DefaultNotifyScope {
    /// Private chats (`inputNotifyUsers` / `notifyUsers`).
    Users,
    /// Basic groups & supergroups/megagroups (`inputNotifyChats` / `notifyChats`).
    Chats,
    /// Broadcast channels (`inputNotifyBroadcasts` / `notifyBroadcasts`).
    Broadcasts,
}

impl DefaultNotifyScope {
    /// All three scopes in index order.
    pub const ALL: [Self; 3] = [Self::Users, Self::Chats, Self::Broadcasts];

    /// Classifies `peer` into its default notification scope (`Data::DefaultNotifyType`,
    /// `data/notify/data_notify_settings.cpp:71-77`).
    #[must_use]
    pub fn for_peer(peer: &Peer) -> Self {
        match peer {
            Peer::User(_) => Self::Users,
            Peer::Chat(_) => Self::Chats,
            Peer::Channel(ch) => {
                if ch.is_megagroup() {
                    Self::Chats
                } else {
                    Self::Broadcasts
                }
            }
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Users => 0,
            Self::Chats => 1,
            Self::Broadcasts => 2,
        }
    }
}

/// The three per-type defaults; `None` until loaded (`settingsUnknown()`).
pub(super) type NotifyDefaults = [Option<PeerNotifySettings>; 3];

impl Session {
    /// The per-type default for `scope`, or `None` while it is not loaded yet.
    #[must_use]
    pub fn notify_default(&self, scope: DefaultNotifyScope) -> Option<&PeerNotifySettings> {
        self.notify_defaults[scope.index()].as_ref()
    }

    /// Stores the per-type default for `scope` and re-evaluates the mute state of every
    /// history in that scope that inherits it (`NotifySettings::apply(DefaultNotify)` +
    /// `updateLocal(DefaultNotify)`, `data/notify/data_notify_settings.cpp:166-176, 431-457`).
    ///
    /// Emits [`ChangeEvent::NotifyDefaultsChanged`] when the stored value changes.
    pub fn set_notify_default(
        &mut self,
        scope: DefaultNotifyScope,
        settings: Option<PeerNotifySettings>,
        now: TimeId,
    ) {
        let slot = &mut self.notify_defaults[scope.index()];
        if *slot == settings {
            return;
        }
        *slot = settings;
        let inheriting: Vec<PeerId> = self
            .histories
            .keys()
            .copied()
            .filter(|pid| {
                self.peers.get(pid).is_some_and(|p| {
                    DefaultNotifyScope::for_peer(p) == scope
                        && p.common().notify.mute_until.is_none()
                })
            })
            .collect();
        for pid in inheriting {
            let muted = self.is_peer_notify_muted(pid, now);
            self.set_history_muted(pid, muted);
        }
        self.changes.send(ChangeEvent::NotifyDefaultsChanged);
    }

    /// Peers whose own notification settings override the per-type default in any field
    /// (chat exceptions, muted authors of mentions).
    pub fn peers_with_notify_settings(&self) -> impl Iterator<Item = PeerId> + '_ {
        self.peers
            .iter()
            .filter(|(_, peer)| peer.common().notify != PeerNotifySettings::default())
            .map(|(id, _)| *id)
    }

    /// Whether `peer_id` is muted at `now`: its own `mute_until`, else its scope default
    /// (`NotifySettings::isMuted(peer)`, `data/notify/data_notify_settings.cpp:547-561`).
    ///
    /// Unlike tdesktop, an unloaded default reads as "not muted" here: this drives the chat list
    /// mute icon and the unread badge, which must not grey out every chat before the defaults
    /// arrive. The notification system keeps tdesktop's "wait until known" rule on its own.
    #[must_use]
    pub fn is_peer_notify_muted(&self, peer_id: PeerId, now: TimeId) -> bool {
        let Some(peer) = self.peers.get(&peer_id) else {
            return false;
        };
        if let Some(until) = peer.common().notify.mute_until {
            return until > now;
        }
        self.notify_default(DefaultNotifyScope::for_peer(peer))
            .and_then(|d| d.mute_until)
            .is_some_and(|until| until > now)
    }

    /// Whether `peer_id`'s own notification settings were received from the server or set
    /// locally (`!peer->notify().settingsUnknown()`). Settings that were never received read as
    /// "inherits the per-type default", which may differ from what the server holds.
    #[must_use]
    pub fn peer_notify_known(&self, peer_id: PeerId) -> bool {
        self.notify_known.contains(&peer_id)
    }

    /// Replaces `peer_id`'s own notification settings, updates its history mute state and
    /// emits `PeerUpdated { NOTIFICATIONS }` when anything changed (`NotifySettings::apply(peer)`
    /// + `updateLocal(peer)`, `data/notify/data_notify_settings.cpp:122-165, 369-382`).
    pub fn set_peer_notify(&mut self, peer_id: PeerId, settings: PeerNotifySettings, now: TimeId) {
        self.notify_known.insert(peer_id);
        let changed = match self.peer_mut(peer_id) {
            Some(peer) if peer.common().notify != settings => {
                peer.common_mut().notify = settings;
                true
            }
            _ => false,
        };
        let muted = self.is_peer_notify_muted(peer_id, now);
        let was_muted = self.histories.get(&peer_id).is_some_and(|h| h.muted);
        // `set_history_muted` emits `NOTIFICATIONS` itself when the mute state flips.
        self.set_history_muted(peer_id, muted);
        if changed && was_muted == muted {
            self.changes
                .peer_updated(peer_id, PeerUpdateFlags::NOTIFICATIONS);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{ChatId, UserId};
    use crate::peer::{Chat, User};

    const NOW: TimeId = 1_700_000_000;

    fn muted(until: TimeId) -> PeerNotifySettings {
        PeerNotifySettings {
            mute_until: Some(until),
            ..PeerNotifySettings::default()
        }
    }

    fn drain(rx: &mut tokio::sync::broadcast::Receiver<ChangeEvent>) -> Vec<ChangeEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
        out
    }

    fn session_with_group() -> (Session, PeerId) {
        let mut s = Session::new(UserId(1));
        s.upsert_user(User::new(UserId(1), true), PeerUpdateFlags::empty());
        let group = PeerId::from_chat(ChatId(5));
        s.upsert_chat(Chat::new(ChatId(5)), PeerUpdateFlags::empty());
        s.ensure_history(group);
        (s, group)
    }

    #[test]
    fn peer_settings_change_is_announced_even_when_the_mute_state_stays() {
        let (mut s, group) = session_with_group();
        let mut rx = s.subscribe();
        s.set_peer_notify(group, muted(NOW + 60), NOW);
        assert!(s.history(group).is_some_and(|h| h.muted));
        // Still muted, but until a different time: subscribers must hear about it.
        let _ = drain(&mut rx);
        s.set_peer_notify(group, muted(i32::MAX), NOW);
        let events = drain(&mut rx);
        assert!(events.iter().any(|ev| matches!(
            ev,
            ChangeEvent::PeerUpdated { peer_id, flags }
                if *peer_id == group && flags.contains(PeerUpdateFlags::NOTIFICATIONS)
        )));
        // Same settings again: nothing to announce.
        s.set_peer_notify(group, muted(i32::MAX), NOW);
        assert!(drain(&mut rx).is_empty());
    }

    #[test]
    fn per_type_default_mutes_only_inheriting_chats_of_its_scope() {
        let (mut s, group) = session_with_group();
        let other = PeerId::from_chat(ChatId(6));
        s.upsert_chat(Chat::new(ChatId(6)), PeerUpdateFlags::empty());
        s.set_peer_notify(other, muted(0), NOW);
        let dm = PeerId::from_user(UserId(2));
        s.upsert_user(User::new(UserId(2), false), PeerUpdateFlags::empty());
        s.ensure_history(dm);
        let mut rx = s.subscribe();

        assert!(
            !s.is_peer_notify_muted(group, NOW),
            "unknown default reads as unmuted"
        );
        s.set_notify_default(DefaultNotifyScope::Chats, Some(muted(i32::MAX)), NOW);
        assert!(s.is_peer_notify_muted(group, NOW));
        assert!(s.history(group).is_some_and(|h| h.muted));
        assert!(!s.is_peer_notify_muted(other, NOW), "explicit unmute wins");
        assert!(!s.is_peer_notify_muted(dm, NOW), "other scope untouched");
        assert!(drain(&mut rx).contains(&ChangeEvent::NotifyDefaultsChanged));
        assert_eq!(
            s.peers_with_notify_settings().collect::<Vec<_>>(),
            vec![other],
            "only peers with their own settings"
        );
    }
}
