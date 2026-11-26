//! Notification preferences, per-peer / per-scope mute settings, and unread badge rules.
//!
//! Port of `Telegram/SourceFiles/data/notify/data_peer_notify_settings.h`,
//! `Telegram/SourceFiles/data/notify/data_peer_notify_settings.cpp`,
//! `Telegram/SourceFiles/data/notify/data_notify_settings.h`,
//! `Telegram/SourceFiles/data/notify/data_notify_settings.cpp`,
//! `Telegram/SourceFiles/core/core_settings.h` (notification settings),
//! and `Telegram/SourceFiles/data/data_session.cpp` (`computeUnreadBadge` / `computeUnreadBadgeMuted`).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::stand_in::types::{MsgId, PeerId, PeerKind};

/// Maximum delay between unmute expiration checks (24 hours in ms, `kMaxNotifyCheckDelay`).
pub const MAX_NOTIFY_CHECK_DELAY_MS: i64 = 24 * 3600 * 1000;

/// Duration of the tray "Mute for 1 hour" shortcut (seconds).
pub const MUTE_ONE_HOUR_SECS: i64 = 3600;

/// Minimum allowed custom notification count.
pub const MIN_NOTIFICATIONS_COUNT: usize = 1;

/// Maximum allowed custom notification count.
pub const MAX_NOTIFICATIONS_COUNT: usize = 5;

/// Default simultaneous custom notification count (`_notificationsCount = 3` in `core_settings.h`).
pub const DEFAULT_NOTIFICATIONS_COUNT: usize = 3;

/// Per-scope default notification category (`Data::DefaultNotify` in `data_peer_notify_settings.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum DefaultNotify {
    /// Private 1-on-1 user chats (`MTP_inputNotifyUsers`).
    User = 0,
    /// Legacy groups and supergroups (`MTP_inputNotifyChats`).
    Group = 1,
    /// Broadcast channels (`MTP_inputNotifyBroadcasts`).
    Broadcast = 2,
}

impl DefaultNotify {
    /// All three default notification scopes in canonical index order.
    pub const ALL: [Self; 3] = [Self::User, Self::Group, Self::Broadcast];

    /// Maps a [`PeerKind`] to its [`DefaultNotify`] scope (`DefaultNotifyType` in `data_notify_settings.cpp`).
    #[must_use]
    pub const fn from_peer_kind(kind: PeerKind) -> Self {
        match kind {
            PeerKind::User => Self::User,
            PeerKind::Group => Self::Group,
            PeerKind::Channel => Self::Broadcast,
        }
    }

    /// Maps a [`PeerId`] to its [`DefaultNotify`] scope.
    #[must_use]
    pub const fn from_peer_id(peer_id: PeerId) -> Self {
        Self::from_peer_kind(peer_id.kind())
    }

    const fn index(self) -> usize {
        self as usize
    }
}

/// Notification sound specification (`Data::NotifySound` in `data_peer_notify_settings.h`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NotifySound {
    /// Local OS sound title, if using a platform sound.
    pub title: String,
    /// Local OS sound identifier/data, if using a platform sound.
    pub data: String,
    /// Custom uploaded ringtone document ID (`0` means default sound).
    pub id: u64,
    /// Explicitly silent ("None" sound).
    pub none: bool,
}

impl NotifySound {
    /// Built-in default notification sound.
    #[must_use]
    pub fn default_sound() -> Self {
        Self::default()
    }

    /// Explicitly silent sound (`MTP_notificationSoundNone`).
    #[must_use]
    pub fn silent() -> Self {
        Self {
            none: true,
            ..Self::default()
        }
    }

    /// Custom Telegram cloud ringtone by document ID (`MTP_notificationSoundRingtone`).
    #[must_use]
    pub fn ringtone(document_id: u64) -> Self {
        Self {
            id: document_id,
            ..Self::default()
        }
    }

    /// Local sound reference (`MTP_notificationSoundLocal`).
    #[must_use]
    pub fn local(title: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            data: data.into(),
            id: 0,
            none: false,
        }
    }
}

/// Mute duration instruction (`Data::MuteValue` in `data_peer_notify_settings.h`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MuteValue {
    /// Explicitly unmute (`mute_until = 0`).
    pub unmute: bool,
    /// Mute indefinitely (`mute_until = i32::MAX`).
    pub forever: bool,
    /// Mute for `period` seconds from now (`period > 0`).
    pub period: i32,
}

impl MuteValue {
    /// No-op mute change (leaves current setting untouched).
    #[must_use]
    pub const fn unchanged() -> Self {
        Self {
            unmute: false,
            forever: false,
            period: 0,
        }
    }

    /// Explicitly unmute.
    #[must_use]
    pub const fn unmute() -> Self {
        Self {
            unmute: true,
            forever: false,
            period: 0,
        }
    }

    /// Mute forever (`i32::MAX`).
    #[must_use]
    pub const fn forever() -> Self {
        Self {
            unmute: false,
            forever: true,
            period: 0,
        }
    }

    /// Mute for `seconds` from current unix time.
    #[must_use]
    pub const fn for_seconds(seconds: i32) -> Self {
        Self {
            unmute: false,
            forever: false,
            period: seconds,
        }
    }

    /// Returns `true` if this value specifies a mute change (`explicit operator bool()`).
    #[must_use]
    pub const fn is_set(self) -> bool {
        self.unmute || self.forever || self.period != 0
    }

    /// Computes the resulting `mute_until` Unix timestamp (`MuteValue::until()`).
    #[must_use]
    pub fn until(self, now_unix: i64) -> i32 {
        if self.forever {
            i32::MAX
        } else if self.period > 0 {
            let target = now_unix.saturating_add(i64::from(self.period));
            target.clamp(0, i64::from(i32::MAX)) as i32
        } else if self.unmute {
            0
        } else {
            -1
        }
    }
}

/// Effective fields inside a [`PeerNotifySettings`] entry (`NotifyPeerSettingsValue`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerNotifySettingsValue {
    /// Unix timestamp until which notifications are muted (`0` = unmuted, `i32::MAX` = forever).
    pub mute_until: Option<i32>,
    /// Sound override for this peer/scope.
    pub sound: Option<NotifySound>,
    /// Whether message previews are shown in notifications.
    pub show_previews: Option<bool>,
    /// Whether channel posts are silent by default.
    pub silent_posts: Option<bool>,
    /// Whether story notifications are muted.
    pub stories_muted: Option<bool>,
}

/// Per-peer, per-topic, or per-scope notification settings (`Data::PeerNotifySettings`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerNotifySettings {
    known: bool,
    value: Option<PeerNotifySettingsValue>,
}

impl PeerNotifySettings {
    /// Creates an unknown (unloaded) settings entry.
    #[must_use]
    pub fn unknown() -> Self {
        Self {
            known: false,
            value: None,
        }
    }

    /// Creates a known entry inheriting all fields from scope defaults.
    #[must_use]
    pub fn inherited_default() -> Self {
        Self {
            known: true,
            value: None,
        }
    }

    /// Creates a known entry with explicit field values.
    #[must_use]
    pub fn with_value(value: PeerNotifySettingsValue) -> Self {
        let is_empty = value.mute_until.is_none()
            && value.sound.is_none()
            && value.show_previews.is_none()
            && value.silent_posts.is_none()
            && value.stories_muted.is_none();
        Self {
            known: true,
            value: if is_empty { None } else { Some(value) },
        }
    }

    /// Returns `true` if settings have not yet been loaded (`settingsUnknown()`).
    #[must_use]
    pub const fn settings_unknown(&self) -> bool {
        !self.known
    }

    /// Explicit `mute_until` override, if set.
    #[must_use]
    pub fn mute_until(&self) -> Option<i32> {
        self.value.as_ref().and_then(|v| v.mute_until)
    }

    /// Explicit `show_previews` override, if set.
    #[must_use]
    pub fn show_previews(&self) -> Option<bool> {
        self.value.as_ref().and_then(|v| v.show_previews)
    }

    /// Explicit `silent_posts` override, if set.
    #[must_use]
    pub fn silent_posts(&self) -> Option<bool> {
        self.value.as_ref().and_then(|v| v.silent_posts)
    }

    /// Explicit `sound` override, if set.
    #[must_use]
    pub fn sound(&self) -> Option<&NotifySound> {
        self.value.as_ref().and_then(|v| v.sound.as_ref())
    }

    /// Explicit `stories_muted` override, if set.
    #[must_use]
    pub fn stories_muted(&self) -> Option<bool> {
        self.value.as_ref().and_then(|v| v.stories_muted)
    }

    /// Applies an incremental settings update. Returns `true` if any field changed.
    pub fn change(
        &mut self,
        mute: MuteValue,
        silent_posts: Option<bool>,
        sound: Option<NotifySound>,
        show_previews: Option<bool>,
        stories_muted: Option<bool>,
        now_unix: i64,
    ) -> bool {
        if !mute.is_set()
            && silent_posts.is_none()
            && sound.is_none()
            && show_previews.is_none()
            && stories_muted.is_none()
        {
            return false;
        }
        let mut next = self.value.clone().unwrap_or_default();
        if mute.is_set() {
            next.mute_until = Some(mute.until(now_unix));
        }
        if let Some(sp) = silent_posts {
            next.silent_posts = Some(sp);
        }
        if let Some(s) = sound {
            next.sound = Some(s);
        }
        if let Some(p) = show_previews {
            next.show_previews = Some(p);
        }
        if let Some(sm) = stories_muted {
            next.stories_muted = Some(sm);
        }
        let changed = !self.known || self.value.as_ref() != Some(&next);
        self.known = true;
        self.value = Some(next);
        changed
    }

    /// Replaces this entry with a server-received snapshot. Returns `true` if changed.
    pub fn apply_snapshot(&mut self, snapshot: Option<PeerNotifySettingsValue>) -> bool {
        let normalized = snapshot.and_then(|v| {
            let empty = v.mute_until.is_none()
                && v.sound.is_none()
                && v.show_previews.is_none()
                && v.silent_posts.is_none()
                && v.stories_muted.is_none();
            if empty { None } else { Some(v) }
        });
        let changed = !self.known || self.value != normalized;
        self.known = true;
        self.value = normalized;
        changed
    }

    /// Resets overrides back to scope defaults (`resetToDefault()`). Returns `true` if changed.
    pub fn reset_to_default(&mut self) -> bool {
        if self.known && self.value.is_none() {
            return false;
        }
        self.known = true;
        self.value = None;
        true
    }
}

/// Evaluates whether `mute_until` is still active at `now_unix`, optionally computing
/// the delay (ms) until expiration (`MutedFromUntil` in `data_notify_settings.cpp`).
#[must_use]
pub fn muted_from_until(until: i32, now_unix: i64) -> (bool, i64) {
    let remaining_secs = i64::from(until).saturating_sub(now_unix);
    if remaining_secs > 0 {
        let delay_ms = remaining_secs
            .saturating_mul(1000)
            .min(MAX_NOTIFY_CHECK_DELAY_MS);
        (true, delay_ms)
    } else {
        (false, MAX_NOTIFY_CHECK_DELAY_MS)
    }
}

/// Identifier for a forum topic notification setting override.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TopicNotifyKey {
    /// Forum peer ID.
    pub peer_id: PeerId,
    /// Forum topic root message ID.
    pub topic_root_id: MsgId,
}

/// Complete notification settings store for an account (`Data::NotifySettings`).
///
/// Manages:
/// - Per-scope defaults for Private chats (`User`), Groups (`Group`), and Channels (`Broadcast`)
/// - Per-peer overrides (tracked in per-scope `exceptions` sets)
/// - Per-forum-topic overrides (falling back to parent peer, then community, then scope default)
/// - Per-peer / per-scope ringtone volume overrides (`0..=100`)
/// - Timed unmute tracking (`unmute_by_finished`)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotifySettingsStore {
    defaults: [PeerNotifySettings; 3],
    peers: BTreeMap<i64, PeerNotifySettings>,
    topics: BTreeMap<(i64, i32), PeerNotifySettings>,
    peer_kinds: BTreeMap<i64, PeerKind>,
    peer_communities: BTreeMap<i64, PeerId>,
    exceptions: [BTreeSet<i64>; 3],
    muted_peers: BTreeSet<i64>,
    muted_topics: BTreeSet<(i64, i32)>,
    peer_volumes: BTreeMap<i64, u8>,
    scope_volumes: [u8; 3],
}

impl Default for NotifySettingsStore {
    fn default() -> Self {
        // Sensible defaults matching a signed-in Telegram account:
        // Private chats & groups unmuted by default; broadcast channels unmuted by default.
        let unmuted_default = PeerNotifySettings::with_value(PeerNotifySettingsValue {
            mute_until: Some(0),
            sound: Some(NotifySound::default_sound()),
            show_previews: Some(true),
            silent_posts: Some(false),
            stories_muted: Some(false),
        });
        Self {
            defaults: [
                unmuted_default.clone(),
                unmuted_default.clone(),
                unmuted_default,
            ],
            peers: BTreeMap::new(),
            topics: BTreeMap::new(),
            peer_kinds: BTreeMap::new(),
            peer_communities: BTreeMap::new(),
            exceptions: [BTreeSet::new(), BTreeSet::new(), BTreeSet::new()],
            muted_peers: BTreeSet::new(),
            muted_topics: BTreeSet::new(),
            peer_volumes: BTreeMap::new(),
            scope_volumes: [0, 0, 0],
        }
    }
}

impl NotifySettingsStore {
    /// Creates a store where all scope defaults start unknown (useful for testing `WaitForSettings`).
    #[must_use]
    pub fn empty_unknown() -> Self {
        Self {
            defaults: [
                PeerNotifySettings::unknown(),
                PeerNotifySettings::unknown(),
                PeerNotifySettings::unknown(),
            ],
            peers: BTreeMap::new(),
            topics: BTreeMap::new(),
            peer_kinds: BTreeMap::new(),
            peer_communities: BTreeMap::new(),
            exceptions: [BTreeSet::new(), BTreeSet::new(), BTreeSet::new()],
            muted_peers: BTreeSet::new(),
            muted_topics: BTreeSet::new(),
            peer_volumes: BTreeMap::new(),
            scope_volumes: [0, 0, 0],
        }
    }

    /// Registers a peer's [`PeerKind`] (important for supergroups encoded as channel IDs).
    pub fn register_peer_kind(&mut self, peer_id: PeerId, kind: PeerKind) {
        self.peer_kinds.insert(peer_id.0, kind);
        self.peers
            .entry(peer_id.0)
            .or_insert_with(PeerNotifySettings::inherited_default);
    }

    /// Links a channel peer to a parent community peer for notification inheritance fallback.
    pub fn set_peer_community(&mut self, peer_id: PeerId, community_id: Option<PeerId>) {
        match community_id {
            Some(cid) => {
                self.peer_communities.insert(peer_id.0, cid);
            }
            None => {
                self.peer_communities.remove(&peer_id.0);
            }
        }
    }

    /// Returns the [`DefaultNotify`] scope for `peer_id`.
    #[must_use]
    pub fn scope_for_peer(&self, peer_id: PeerId) -> DefaultNotify {
        self.peer_kinds
            .get(&peer_id.0)
            .copied()
            .map(DefaultNotify::from_peer_kind)
            .unwrap_or_else(|| DefaultNotify::from_peer_id(peer_id))
    }

    /// Returns the scope default settings (`defaultSettings(DefaultNotify)`).
    #[must_use]
    pub fn default_settings(&self, scope: DefaultNotify) -> &PeerNotifySettings {
        &self.defaults[scope.index()]
    }

    /// Returns `true` if a scope default is muted at `now_unix`.
    #[must_use]
    pub fn is_scope_muted(&self, scope: DefaultNotify, now_unix: i64) -> bool {
        match self.default_settings(scope).mute_until() {
            Some(until) => muted_from_until(until, now_unix).0,
            None => true,
        }
    }

    /// Updates a per-scope default (`defaultUpdate`). Returns `true` if changed.
    #[allow(clippy::too_many_arguments)]
    pub fn update_default(
        &mut self,
        scope: DefaultNotify,
        mute: MuteValue,
        silent_posts: Option<bool>,
        sound: Option<NotifySound>,
        show_previews: Option<bool>,
        stories_muted: Option<bool>,
        now_unix: i64,
    ) -> bool {
        self.defaults[scope.index()].change(
            mute,
            silent_posts,
            sound,
            show_previews,
            stories_muted,
            now_unix,
        )
    }

    /// Applies a full snapshot to a per-scope default.
    pub fn apply_default(
        &mut self,
        scope: DefaultNotify,
        snapshot: Option<PeerNotifySettingsValue>,
    ) -> bool {
        self.defaults[scope.index()].apply_snapshot(snapshot)
    }

    /// Updates a peer's notification settings and refreshes its exception membership.
    #[allow(clippy::too_many_arguments)]
    pub fn update_peer(
        &mut self,
        peer_id: PeerId,
        mute: MuteValue,
        silent_posts: Option<bool>,
        sound: Option<NotifySound>,
        show_previews: Option<bool>,
        stories_muted: Option<bool>,
        now_unix: i64,
    ) -> bool {
        let entry = self
            .peers
            .entry(peer_id.0)
            .or_insert_with(PeerNotifySettings::inherited_default);
        let changed = entry.change(
            mute,
            silent_posts,
            sound,
            show_previews,
            stories_muted,
            now_unix,
        );
        if changed {
            self.refresh_peer_tracking(peer_id, now_unix);
        }
        changed
    }

    /// Applies a server snapshot to a peer's notification settings.
    pub fn apply_peer(
        &mut self,
        peer_id: PeerId,
        snapshot: Option<PeerNotifySettingsValue>,
        now_unix: i64,
    ) -> bool {
        let entry = self
            .peers
            .entry(peer_id.0)
            .or_insert_with(PeerNotifySettings::unknown);
        let changed = entry.apply_snapshot(snapshot);
        if changed {
            self.refresh_peer_tracking(peer_id, now_unix);
        }
        changed
    }

    /// Resets a peer back to its scope default (`resetToDefault(peer)`).
    pub fn reset_peer_to_default(&mut self, peer_id: PeerId, now_unix: i64) -> bool {
        let entry = self
            .peers
            .entry(peer_id.0)
            .or_insert_with(PeerNotifySettings::inherited_default);
        let changed = entry.reset_to_default();
        if changed {
            self.refresh_peer_tracking(peer_id, now_unix);
        }
        changed
    }

    /// Updates a forum topic's notification settings.
    pub fn update_topic(
        &mut self,
        peer_id: PeerId,
        topic_root_id: MsgId,
        mute: MuteValue,
        sound: Option<NotifySound>,
        now_unix: i64,
    ) -> bool {
        let key = (peer_id.0, topic_root_id.0);
        let entry = self
            .topics
            .entry(key)
            .or_insert_with(PeerNotifySettings::inherited_default);
        let changed = entry.change(mute, None, sound, None, None, now_unix);
        if changed {
            if self.is_topic_muted(peer_id, topic_root_id, now_unix) {
                self.muted_topics.insert(key);
            } else {
                self.muted_topics.remove(&key);
            }
        }
        changed
    }

    /// Resets a forum topic back to its parent peer settings.
    pub fn reset_topic_to_default(
        &mut self,
        peer_id: PeerId,
        topic_root_id: MsgId,
        now_unix: i64,
    ) -> bool {
        let key = (peer_id.0, topic_root_id.0);
        let entry = self
            .topics
            .entry(key)
            .or_insert_with(PeerNotifySettings::inherited_default);
        let changed = entry.reset_to_default();
        if changed {
            if self.is_topic_muted(peer_id, topic_root_id, now_unix) {
                self.muted_topics.insert(key);
            } else {
                self.muted_topics.remove(&key);
            }
        }
        changed
    }

    /// Applies a server snapshot to a forum topic's notification settings
    /// (`NotifySettings::apply(topic)`). Returns `true` if changed.
    pub fn apply_topic(
        &mut self,
        peer_id: PeerId,
        topic_root_id: MsgId,
        snapshot: Option<PeerNotifySettingsValue>,
        now_unix: i64,
    ) -> bool {
        let key = (peer_id.0, topic_root_id.0);
        let changed = self
            .topics
            .entry(key)
            .or_insert_with(PeerNotifySettings::unknown)
            .apply_snapshot(snapshot);
        if changed {
            if self.is_topic_muted(peer_id, topic_root_id, now_unix) {
                self.muted_topics.insert(key);
            } else {
                self.muted_topics.remove(&key);
            }
        }
        changed
    }

    /// Marks a peer's settings explicitly unknown (useful for testing delayed resolution).
    pub fn mark_peer_unknown(&mut self, peer_id: PeerId) {
        self.peers.insert(peer_id.0, PeerNotifySettings::unknown());
    }

    /// Returns whether `peer_id`'s mute state is unknown (`muteUnknown(peer)`).
    #[must_use]
    pub fn mute_unknown(&self, peer_id: PeerId) -> bool {
        let Some(peer_settings) = self.peers.get(&peer_id.0) else {
            return self
                .default_settings(self.scope_for_peer(peer_id))
                .settings_unknown();
        };
        if peer_settings.settings_unknown() {
            return true;
        }
        if peer_settings.mute_until().is_some() {
            return false;
        }
        if let Some(community_id) = self.peer_communities.get(&peer_id.0)
            && let Some(comm) = self.peers.get(&community_id.0)
        {
            return comm.settings_unknown()
                || (comm.mute_until().is_none()
                    && self
                        .default_settings(self.scope_for_peer(peer_id))
                        .settings_unknown());
        }
        self.default_settings(self.scope_for_peer(peer_id))
            .settings_unknown()
    }

    /// Returns whether a topic's mute state is unknown (`muteUnknown(thread)`).
    #[must_use]
    pub fn topic_mute_unknown(&self, peer_id: PeerId, topic_root_id: MsgId) -> bool {
        if topic_root_id.0 == 0 {
            return self.mute_unknown(peer_id);
        }
        if let Some(topic) = self.topics.get(&(peer_id.0, topic_root_id.0)) {
            if topic.settings_unknown() {
                return true;
            }
            if topic.mute_until().is_some() {
                return false;
            }
        }
        self.mute_unknown(peer_id)
    }

    /// Evaluates whether `peer_id` is muted at `now_unix`, following the exact
    /// peer -> community -> scope-default precedence from `data_notify_settings.cpp:547-561`.
    #[must_use]
    pub fn is_peer_muted(&self, peer_id: PeerId, now_unix: i64) -> bool {
        self.is_peer_muted_with_delay(peer_id, now_unix).0
    }

    /// Evaluates whether `peer_id` is muted and returns the ms delay until mute expires.
    #[must_use]
    pub fn is_peer_muted_with_delay(&self, peer_id: PeerId, now_unix: i64) -> (bool, i64) {
        if let Some(until) = self
            .peers
            .get(&peer_id.0)
            .and_then(PeerNotifySettings::mute_until)
        {
            return muted_from_until(until, now_unix);
        }
        if let Some(community_id) = self.peer_communities.get(&peer_id.0)
            && let Some(until) = self
                .peers
                .get(&community_id.0)
                .and_then(PeerNotifySettings::mute_until)
        {
            return muted_from_until(until, now_unix);
        }
        if let Some(until) = self
            .default_settings(self.scope_for_peer(peer_id))
            .mute_until()
        {
            return muted_from_until(until, now_unix);
        }
        (true, MAX_NOTIFY_CHECK_DELAY_MS)
    }

    /// Evaluates whether a topic (or chat if `topic_root_id == 0`) is muted at `now_unix`.
    #[must_use]
    pub fn is_topic_muted(&self, peer_id: PeerId, topic_root_id: MsgId, now_unix: i64) -> bool {
        if topic_root_id.0 != 0
            && let Some(until) = self
                .topics
                .get(&(peer_id.0, topic_root_id.0))
                .and_then(PeerNotifySettings::mute_until)
        {
            return muted_from_until(until, now_unix).0;
        }
        self.is_peer_muted(peer_id, now_unix)
    }

    /// Resolves effective [`NotifySound`] for a peer (ignoring overrides on Saved Messages,
    /// matching `data_notify_settings.cpp:581-595`).
    #[must_use]
    pub fn sound_for_peer(&self, peer_id: PeerId, is_self_chat: bool) -> NotifySound {
        if !is_self_chat
            && let Some(sound) = self
                .peers
                .get(&peer_id.0)
                .and_then(PeerNotifySettings::sound)
        {
            return sound.clone();
        }
        if let Some(community_id) = self.peer_communities.get(&peer_id.0)
            && let Some(sound) = self
                .peers
                .get(&community_id.0)
                .and_then(PeerNotifySettings::sound)
        {
            return sound.clone();
        }
        if let Some(sound) = self.default_settings(self.scope_for_peer(peer_id)).sound() {
            return sound.clone();
        }
        NotifySound::default_sound()
    }

    /// Resolves effective [`NotifySound`] for a forum topic or peer.
    #[must_use]
    pub fn sound_for_topic(
        &self,
        peer_id: PeerId,
        topic_root_id: MsgId,
        is_self_chat: bool,
    ) -> NotifySound {
        if topic_root_id.0 != 0
            && let Some(sound) = self
                .topics
                .get(&(peer_id.0, topic_root_id.0))
                .and_then(PeerNotifySettings::sound)
        {
            return sound.clone();
        }
        self.sound_for_peer(peer_id, is_self_chat)
    }

    /// Resolves effective `show_previews` for a peer (default `true`).
    #[must_use]
    pub fn show_previews_for_peer(&self, peer_id: PeerId) -> bool {
        if let Some(v) = self
            .peers
            .get(&peer_id.0)
            .and_then(PeerNotifySettings::show_previews)
        {
            return v;
        }
        if let Some(v) = self
            .default_settings(self.scope_for_peer(peer_id))
            .show_previews()
        {
            return v;
        }
        true
    }

    /// Resolves effective `silent_posts` for a channel peer (default `false`).
    #[must_use]
    pub fn silent_posts_for_peer(&self, peer_id: PeerId) -> bool {
        if let Some(v) = self
            .peers
            .get(&peer_id.0)
            .and_then(PeerNotifySettings::silent_posts)
        {
            return v;
        }
        if let Some(community_id) = self.peer_communities.get(&peer_id.0)
            && let Some(v) = self
                .peers
                .get(&community_id.0)
                .and_then(PeerNotifySettings::silent_posts)
        {
            return v;
        }
        if let Some(v) = self
            .default_settings(self.scope_for_peer(peer_id))
            .silent_posts()
        {
            return v;
        }
        false
    }

    /// Sets a per-peer ringtone volume override (`0` = inherit scope/global, `1..=100` = explicit %).
    pub fn set_peer_volume(&mut self, peer_id: PeerId, volume_pct: u8) {
        let clamped = volume_pct.min(100);
        if clamped == 0 {
            self.peer_volumes.remove(&peer_id.0);
        } else {
            self.peer_volumes.insert(peer_id.0, clamped);
        }
    }

    /// Sets a per-scope ringtone volume override (`0` = inherit global, `1..=100` = explicit %).
    pub fn set_scope_volume(&mut self, scope: DefaultNotify, volume_pct: u8) {
        self.scope_volumes[scope.index()] = volume_pct.min(100);
    }

    /// Resolves ringtone volume for a peer (`Some(pct)` if overridden, `None` for global default).
    #[must_use]
    pub fn ringtone_volume_override(&self, peer_id: PeerId) -> Option<u8> {
        if let Some(&v) = self.peer_volumes.get(&peer_id.0)
            && v > 0
        {
            return Some(v);
        }
        let scope_v = self.scope_volumes[self.scope_for_peer(peer_id).index()];
        if scope_v > 0 { Some(scope_v) } else { None }
    }

    /// Returns all peer IDs that have explicit mute exceptions in `scope`.
    #[must_use]
    pub fn exceptions(&self, scope: DefaultNotify) -> Vec<PeerId> {
        self.exceptions[scope.index()]
            .iter()
            .map(|&id| PeerId(id))
            .collect()
    }

    /// Clears all peer notification exceptions for `scope` (`clearExceptions`).
    pub fn clear_exceptions(&mut self, scope: DefaultNotify, now_unix: i64) -> Vec<PeerId> {
        let cleared: Vec<PeerId> = std::mem::take(&mut self.exceptions[scope.index()])
            .into_iter()
            .map(PeerId)
            .collect();
        for &peer_id in &cleared {
            if let Some(entry) = self.peers.get_mut(&peer_id.0) {
                entry.reset_to_default();
            }
            if self.is_peer_muted(peer_id, now_unix) {
                self.muted_peers.insert(peer_id.0);
            } else {
                self.muted_peers.remove(&peer_id.0);
            }
        }
        cleared
    }

    /// Sweeps expired temporary mutes at `now_unix` (`unmuteByFinished`).
    ///
    /// Returns `(unmuted_peers, unmuted_topics, next_check_delay_ms)`.
    pub fn unmute_by_finished(
        &mut self,
        now_unix: i64,
    ) -> (Vec<PeerId>, Vec<TopicNotifyKey>, Option<i64>) {
        let mut unmuted_peers = Vec::new();
        let mut unmuted_topics = Vec::new();
        let mut min_delay: Option<i64> = None;

        let tracked_peers: Vec<i64> = self.muted_peers.iter().copied().collect();
        for raw_id in tracked_peers {
            let (muted, delay) = self.is_peer_muted_with_delay(PeerId(raw_id), now_unix);
            if muted {
                min_delay = Some(min_delay.map_or(delay, |m| m.min(delay)));
            } else {
                self.muted_peers.remove(&raw_id);
                unmuted_peers.push(PeerId(raw_id));
            }
        }

        let tracked_topics: Vec<(i64, i32)> = self.muted_topics.iter().copied().collect();
        for (raw_peer, raw_topic) in tracked_topics {
            let until_opt = self
                .topics
                .get(&(raw_peer, raw_topic))
                .and_then(PeerNotifySettings::mute_until);
            let (muted, delay) = match until_opt {
                Some(until) => muted_from_until(until, now_unix),
                None => self.is_peer_muted_with_delay(PeerId(raw_peer), now_unix),
            };
            if muted {
                min_delay = Some(min_delay.map_or(delay, |m| m.min(delay)));
            } else {
                self.muted_topics.remove(&(raw_peer, raw_topic));
                unmuted_topics.push(TopicNotifyKey {
                    peer_id: PeerId(raw_peer),
                    topic_root_id: MsgId(raw_topic),
                });
            }
        }

        (unmuted_peers, unmuted_topics, min_delay)
    }

    fn refresh_peer_tracking(&mut self, peer_id: PeerId, now_unix: i64) {
        let scope = self.scope_for_peer(peer_id);
        let has_exception = self
            .peers
            .get(&peer_id.0)
            .and_then(PeerNotifySettings::mute_until)
            .is_some();
        if has_exception {
            self.exceptions[scope.index()].insert(peer_id.0);
        } else {
            self.exceptions[scope.index()].remove(&peer_id.0);
        }
        if self.is_peer_muted(peer_id, now_unix) {
            self.muted_peers.insert(peer_id.0);
        } else {
            self.muted_peers.remove(&peer_id.0);
        }
    }
}

/// Privacy level controlling what fields appear in desktop notifications
/// (`Core::Settings::NotifyView` in `core_settings.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[repr(u8)]
pub enum NotifyView {
    /// Show sender/chat name, avatar, and message preview.
    #[default]
    ShowPreview = 0,
    /// Show sender/chat name and avatar, hide message body ("You have a new message").
    ShowName = 1,
    /// Hide sender name, avatar, and message body.
    ShowNothing = 2,
}

/// Screen corner where custom in-app notification popups stack
/// (`Core::Settings::ScreenCorner` in `core_settings.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[repr(u8)]
pub enum ScreenCorner {
    /// Top-left corner (stack shifts downward).
    TopLeft = 0,
    /// Top-right corner (stack shifts downward).
    TopRight = 1,
    /// Bottom-right corner (default on Windows, stack shifts upward).
    #[default]
    BottomRight = 2,
    /// Bottom-left corner (stack shifts upward).
    BottomLeft = 3,
}

impl ScreenCorner {
    /// Returns `true` for left-side corners (`IsLeftCorner` in `core_settings.h`).
    #[must_use]
    pub const fn is_left(self) -> bool {
        matches!(self, Self::TopLeft | Self::BottomLeft)
    }

    /// Returns `true` for top-side corners (`IsTopCorner` in `core_settings.h`).
    #[must_use]
    pub const fn is_top(self) -> bool {
        matches!(self, Self::TopLeft | Self::TopRight)
    }
}

/// User-facing global notification preferences (`Core::Settings` notification fields).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationPreferences {
    /// Master switch for desktop popup/toast notifications (`desktopNotify`).
    pub desktop_notify: bool,
    /// Play audio chime for incoming notifications (`soundNotify`).
    pub sound_notify: bool,
    /// Flash taskbar button on incoming notifications (`flashBounceNotify`).
    pub flash_bounce_notify: bool,
    /// Privacy level for notification sender name and message preview (`notifyView`).
    pub notify_view: NotifyView,
    /// Prefer native OS (Windows WinRT) toast notifications over custom popups (`nativeNotifications`).
    pub native_notifications: bool,
    /// Override to allow custom notifications even if native is preferred (`kOptionCustomNotification`).
    pub force_custom_notifications: bool,
    /// Maximum simultaneous custom notification popups (`1..=5`, default `3`).
    pub notifications_count: usize,
    /// Screen corner for custom notification popups.
    pub notifications_corner: ScreenCorner,
    /// Include muted chats in the unread taskbar/tray badge (`includeMutedCounter`).
    pub include_muted_counter: bool,
    /// Count unread messages rather than unread chats (`countUnreadMessages`).
    pub count_unread_messages: bool,
    /// Show notifications from all signed-in accounts, not just the active account (`notifyFromAll`).
    pub notify_from_all: bool,
    /// Suppress notifications when Windows Focus Assist / Do Not Disturb is active.
    pub skip_in_focus_assist: bool,
    /// Hide the quick-reply input/button on notifications (`kOptionHideReplyButton`).
    pub hide_reply_button: bool,
    /// Global notification chime volume (`0..=100`).
    pub notifications_volume_pct: u8,
    /// Whether reaction notifications show the reacting user's name.
    pub reaction_show_previews: bool,
    /// Remembered sound toggle when disabling notifications from tray (`rememberedSoundNotifyFromTray`).
    pub remembered_sound_notify_from_tray: bool,
    /// Remembered flash toggle when disabling notifications from tray (`rememberedFlashBounceNotifyFromTray`).
    pub remembered_flash_bounce_notify_from_tray: bool,
    /// Optional temporary global mute expiry timestamp (Unix seconds), used by tray "Mute for 1 hour".
    pub global_mute_until_unix: Option<i64>,
    /// Minimum burst size in the same chat within a single dispatch pass before collapsing to `"N new messages"`.
    pub collapse_burst_threshold: usize,
}

impl Default for NotificationPreferences {
    fn default() -> Self {
        Self {
            desktop_notify: true,
            sound_notify: true,
            flash_bounce_notify: true,
            notify_view: NotifyView::ShowPreview,
            native_notifications: true,
            force_custom_notifications: false,
            notifications_count: DEFAULT_NOTIFICATIONS_COUNT,
            notifications_corner: ScreenCorner::BottomRight,
            include_muted_counter: true,
            count_unread_messages: true,
            notify_from_all: true,
            skip_in_focus_assist: true,
            hide_reply_button: false,
            notifications_volume_pct: 100,
            reaction_show_previews: true,
            remembered_sound_notify_from_tray: false,
            remembered_flash_bounce_notify_from_tray: false,
            global_mute_until_unix: None,
            collapse_burst_threshold: 3,
        }
    }
}

impl NotificationPreferences {
    /// Returns the clamped simultaneous custom notification count (`1..=5`).
    #[must_use]
    pub fn clamped_notifications_count(&self) -> usize {
        self.notifications_count
            .clamp(MIN_NOTIFICATIONS_COUNT, MAX_NOTIFICATIONS_COUNT)
    }

    /// Returns `true` if the temporary global mute ("Mute for 1 hour") is active at `now_unix`.
    #[must_use]
    pub fn is_globally_muted(&self, now_unix: i64) -> bool {
        self.global_mute_until_unix
            .is_some_and(|until| until > now_unix)
    }

    /// Activates global temporary mute for `duration_secs` from `now_unix`.
    pub fn mute_globally_for(&mut self, now_unix: i64, duration_secs: i64) {
        self.global_mute_until_unix = Some(now_unix.saturating_add(duration_secs.max(0)));
    }

    /// Clears any active temporary global mute.
    pub fn clear_global_mute(&mut self) {
        self.global_mute_until_unix = None;
    }

    /// Checks whether a temporary global mute has expired at `now_unix`, clearing it if so.
    /// Returns `true` if a previously active mute just expired.
    pub fn expire_global_mute(&mut self, now_unix: i64) -> bool {
        if let Some(until) = self.global_mute_until_unix
            && until <= now_unix
        {
            self.global_mute_until_unix = None;
            return true;
        }
        false
    }
}

/// Aggregated unread counters across all chats in a session (`Dialogs::UnreadState`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnreadBadgeState {
    /// Total unread messages across all chats (unmuted + muted).
    pub messages: u32,
    /// Unread messages in muted chats.
    pub messages_muted: u32,
    /// Total chats with unread messages.
    pub chats: u32,
    /// Muted chats with unread messages.
    pub chats_muted: u32,
    /// Total chats manually marked as unread.
    pub marks: u32,
    /// Muted chats manually marked as unread.
    pub marks_muted: u32,
    /// Unread mentions count (always unmuted for badge highlighting).
    pub mentions: u32,
}

/// Computed tray/taskbar unread badge result.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UnreadBadgeResult {
    /// Numeric badge value (`0` hides the badge).
    pub count: u32,
    /// `true` when the badge should be rendered in muted gray (`trayCounterBgMute`).
    pub muted: bool,
}

/// Computes the numeric unread badge count (`Data::Session::computeUnreadBadge` in `data_session.cpp:3731-3737`).
#[must_use]
pub fn compute_unread_badge(
    state: UnreadBadgeState,
    include_muted_counter: bool,
    count_unread_messages: bool,
) -> u32 {
    let marks = if include_muted_counter {
        state.marks
    } else {
        state.marks.saturating_sub(state.marks_muted)
    };
    let primary = if count_unread_messages {
        if include_muted_counter {
            state.messages
        } else {
            state.messages.saturating_sub(state.messages_muted)
        }
    } else if include_muted_counter {
        state.chats
    } else {
        state.chats.saturating_sub(state.chats_muted)
    };
    marks.saturating_add(primary)
}

/// Computes whether the unread badge should use the muted gray color
/// (`Data::Session::computeUnreadBadgeMuted` in `data_session.cpp:3739-3748`).
#[must_use]
pub fn compute_unread_badge_muted(
    state: UnreadBadgeState,
    include_muted_counter: bool,
    count_unread_messages: bool,
) -> bool {
    if !include_muted_counter {
        return false;
    }
    let marks_all_muted = state.marks_muted >= state.marks;
    let primary_all_muted = if count_unread_messages {
        state.messages_muted >= state.messages
    } else {
        state.chats_muted >= state.chats
    };
    marks_all_muted && primary_all_muted
}

/// Computes both badge count and muted color flag (taking unread mentions into account
/// per `unreadWithMentionsBadge` / `unreadWithMentionsBadgeMuted` in `data_session.cpp:3671-3686`).
#[must_use]
pub fn compute_badge(
    state: UnreadBadgeState,
    prefs: &NotificationPreferences,
) -> UnreadBadgeResult {
    let mut base = state;
    if base.mentions > 0 {
        base.messages = base.messages.saturating_sub(base.mentions);
    }
    let count = compute_unread_badge(
        base,
        prefs.include_muted_counter,
        prefs.count_unread_messages,
    )
    .saturating_add(state.mentions);
    let muted = state.mentions == 0
        && compute_unread_badge_muted(
            state,
            prefs.include_muted_counter,
            prefs.count_unread_messages,
        );
    UnreadBadgeResult { count, muted }
}

/// Per-account unread state paired with that account's badge-counting preferences.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AccountUnreadEntry {
    /// Raw unread counts across chats for one account.
    pub state: UnreadBadgeState,
    /// Include muted chats in this account's badge (`includeMutedCounter`).
    pub include_muted_counter: bool,
    /// Count unread messages instead of unread chats for this account (`countUnreadMessages`).
    pub count_unread_messages: bool,
}

impl AccountUnreadEntry {
    /// Computes the individual account's unread badge using its own counting preferences.
    #[must_use]
    pub fn compute_badge(self) -> UnreadBadgeResult {
        let mut base = self.state;
        if base.mentions > 0 {
            base.messages = base.messages.saturating_sub(base.mentions);
        }
        let count =
            compute_unread_badge(base, self.include_muted_counter, self.count_unread_messages)
                .saturating_add(self.state.mentions);
        let muted = self.state.mentions == 0
            && compute_unread_badge_muted(
                self.state,
                self.include_muted_counter,
                self.count_unread_messages,
            );
        UnreadBadgeResult { count, muted }
    }
}

/// Sums unread badges across all signed-in accounts, respecting each account's
/// `include_muted_counter` and `count_unread_messages` settings
/// (port of `Main::Domain::updateUnreadBadge` in `main/main_domain.cpp:302-315`).
#[must_use]
pub fn aggregate_account_badges_per_account(
    accounts: impl IntoIterator<Item = AccountUnreadEntry>,
) -> UnreadBadgeResult {
    let mut total_count = 0u32;
    let mut any_unmuted = false;
    for entry in accounts {
        let badge = entry.compute_badge();
        total_count = total_count.saturating_add(badge.count);
        if badge.count > 0 && !badge.muted {
            any_unmuted = true;
        }
    }
    UnreadBadgeResult {
        count: total_count,
        muted: total_count > 0 && !any_unmuted,
    }
}

/// Sums unread badges across all signed-in accounts using shared [`NotificationPreferences`].
#[must_use]
pub fn aggregate_account_badges(
    states: impl IntoIterator<Item = UnreadBadgeState>,
    prefs: &NotificationPreferences,
) -> UnreadBadgeResult {
    aggregate_account_badges_per_account(states.into_iter().map(|state| AccountUnreadEntry {
        state,
        include_muted_counter: prefs.include_muted_counter,
        count_unread_messages: prefs.count_unread_messages,
    }))
}
