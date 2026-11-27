//! Port of `data/data_session.h` and `main/main_session` (data ownership parts).
//!
//! Provides the `Session` registry owning peers, histories, messages, dialog lists,
//! archive folder, chat filters, and `tokio::sync::broadcast` change notifications.

use std::collections::BTreeSet;
use std::sync::Mutex;

use rustc_hash::{FxHashMap, FxHashSet};

use tokio::sync::broadcast;

use crate::changes::{
    ChangeEvent, Changes, DEFAULT_CHANGES_CAPACITY, EntryUpdateFlags, HistoryUpdateFlags,
    MessageUpdateFlags, PeerUpdateFlags, TopicUpdateFlags, TypingAction,
};
use crate::dialogs::{
    BadgeSettings, BadgesState, ChatFilter, ChatFilters, CountInBadge, DialogKey, Folder,
    IncludeInBadge, MainList, UnreadState, badges_for_unread, compute_dialog_sort_key,
};
use crate::forum::{ForumState, ForumTopicData};
use crate::history::{Draft, DraftKey, History, InboxReadOutcome};
use crate::id::{
    ARCHIVE_FOLDER_ID, ChannelId, ChatId, FilterId, FolderId, FullMsgId, MsgId, MsgRange, PeerId,
    PhotoId, UNKNOWN_PHOTO_ID, UserId,
};
use crate::lastseen::{LastseenStatus, UserStatusInput};
use crate::message::{
    Message, MessageContent, MessageFlags, MessageMedia, MessageReactionsSummary, PhotoData,
    ServiceAction,
};
use crate::peer::{Channel, ChannelFlags, Chat, ChatFlags, Peer, User};
use crate::rights::{ChatBannedRights, SendCheckContext};
use crate::saved::{SavedMessagesState, SavedReactionTagsState};
use crate::scheduled::{ScheduledMessagesStore, ShortcutMessagesStore};
use crate::text::EntityKind;

/// Messages kept in memory across all chats before the least recently loaded chats that are
/// not open are unloaded down to their newest messages ([`Session::set_message_budget`]).
pub const DEFAULT_MESSAGE_BUDGET: usize = 30_000;

/// Newest loaded messages an unloaded chat keeps (with all its unread ones): more than a first
/// screen asks for, so reopening it shows the same bottom of the chat at once and only older
/// messages reload from the server on scroll, as on a first open.
pub const UNLOAD_KEEP_TAIL: usize = 200;

/// Default pinned dialogs limit for the main chats list.
pub const DEFAULT_MAIN_PINNED_LIMIT: usize = 5;

/// Default pinned dialogs limit for the archive folder and custom filters.
pub const DEFAULT_FOLDER_PINNED_LIMIT: usize = 100;

mod history_pages;
mod indices;
mod notify_defaults;
mod service_changes;
#[cfg(test)]
mod tests_perf;
mod unload;

pub use history_pages::HistoryPage;
pub use notify_defaults::DefaultNotifyScope;

/// Central TL-independent domain data registry for a Telegram account (`Data::Session`).
#[derive(Debug)]
pub struct Session {
    self_user_id: UserId,
    send_context: SendCheckContext,
    badge_settings: BadgeSettings,
    dialogs_unread_on_top: bool,
    peers: FxHashMap<PeerId, Peer>,
    photos: FxHashMap<PhotoId, PhotoData>,
    access_hashes: FxHashMap<PeerId, i64>,
    /// Tracks server messages that reference an unloaded or `min`-only peer so
    /// `inputPeerUserFromMessage` / `inputPeerChannelFromMessage` can be constructed
    /// (`_messagesWithPeer` in `data/data_session.h` / `data/data_session.cpp:6145-6282`).
    messages_with_peer: FxHashMap<PeerId, Vec<FullMsgId>>,
    contacts_loaded: bool,
    histories: FxHashMap<PeerId, History>,
    messages: FxHashMap<FullMsgId, Message>,
    non_channel_messages: FxHashMap<MsgId, PeerId>,
    /// Histories that got a new message while their folder (main list or archive) is unknown;
    /// their dialog entries must be requested with `messages.getPeerDialogs`
    /// (`Histories::requestDialogEntry` from `History::addNewItem`, `history/history.cpp:1731`).
    dialog_entry_requests: BTreeSet<PeerId>,
    /// Histories read past what loaded messages can count; their dialog entries bring the
    /// server's unread count (`History::inboxRead`, `history/history.cpp:2291-2299`).
    unread_count_requests: BTreeSet<PeerId>,
    random_id_map: FxHashMap<i64, FullMsgId>,
    typing_actions: FxHashMap<(PeerId, PeerId, Option<MsgId>), TypingAction>,
    main_list: MainList,
    archive: Folder,
    filters: ChatFilters,
    forums: FxHashMap<PeerId, ForumState>,
    saved_messages: SavedMessagesState,
    monoforums: FxHashMap<ChannelId, SavedMessagesState>,
    saved_reaction_tags: SavedReactionTagsState,
    scheduled_messages: ScheduledMessagesStore,
    shortcut_messages: ShortcutMessagesStore,
    sort_shift: u32,
    changes: Changes,
    /// Message ids present in `messages`, per peer, so per-chat queries never scan every
    /// loaded message.
    peer_message_ids: FxHashMap<PeerId, BTreeSet<MsgId>>,
    /// Photo / document id -> messages showing it ([`Session::messages_with_media`]).
    media_messages: FxHashMap<u64, Vec<FullMsgId>>,
    /// Reverse of `media_messages`.
    message_media: FxHashMap<FullMsgId, u64>,
    /// Messages handed out by [`Session::message_mut`] since they were last indexed: their
    /// media may have changed, so lookups check them and the next insert or delete reindexes.
    media_dirty: FxHashSet<FullMsgId>,
    /// Bumped by every accessor that can change a peer (usernames, loaded state).
    peers_generation: u64,
    /// Lowercased username -> peer, rebuilt on lookup when `peers_generation` moved.
    username_index: Mutex<UsernameIndex>,
    message_budget: Option<usize>,
    /// Message count above which the next insert tries to unload chats.
    next_unload_check: usize,
    /// Last time each history got a loaded slice or was marked open (`touch_tick` order).
    history_touch: FxHashMap<PeerId, u64>,
    touch_tick: u64,
    /// Histories the UI shows now; never unloaded.
    open_histories: FxHashSet<PeerId>,
    /// Per-type default notification settings ([`Session::notify_default`]).
    notify_defaults: notify_defaults::NotifyDefaults,
    /// Peers whose own notification settings were received ([`Session::peer_notify_known`]).
    notify_known: FxHashSet<PeerId>,
    /// Unloading runs only once the UI reports which chats are open
    /// ([`Session::set_history_open`]) or a budget is set explicitly: a chat reopened from
    /// memory loads no slice, so without that report it could look old while on screen.
    unload_armed: bool,
}

#[derive(Debug, Default)]
struct UsernameIndex {
    generation: Option<u64>,
    by_name: FxHashMap<String, PeerId>,
}

/// Photo or document id a message shows, the key of [`Session::messages_with_media`].
fn message_media_key(message: &Message) -> Option<u64> {
    match message.media()? {
        MessageMedia::Photo(photo) => Some(photo.id.0),
        MessageMedia::Document(document) => Some(document.id.0),
        _ => None,
    }
}

/// Whether a loaded message counts as unread once past the read mark: a regular incoming one
/// (`History::countStillUnreadLocal`, `history/history.cpp:2210-2213, 2242-2246`).
fn is_unread_candidate(messages: &FxHashMap<FullMsgId, Message>, id: FullMsgId) -> bool {
    messages
        .get(&id)
        .is_some_and(|m| m.is_regular() && !m.is_outgoing())
}

impl Session {
    /// Creates a new `Session` for the logged-in account `self_user_id`.
    pub fn new(self_user_id: UserId) -> Self {
        Self::with_broadcast_capacity(self_user_id, DEFAULT_CHANGES_CAPACITY)
    }

    /// Creates a new `Session` with an explicit change-notification channel capacity.
    pub fn with_broadcast_capacity(self_user_id: UserId, broadcast_capacity: usize) -> Self {
        Self {
            self_user_id,
            send_context: SendCheckContext::default(),
            badge_settings: BadgeSettings::default(),
            dialogs_unread_on_top: false,
            peers: FxHashMap::default(),
            photos: FxHashMap::default(),
            access_hashes: FxHashMap::default(),
            messages_with_peer: FxHashMap::default(),
            contacts_loaded: false,
            histories: FxHashMap::default(),
            messages: FxHashMap::default(),
            non_channel_messages: FxHashMap::default(),
            dialog_entry_requests: BTreeSet::new(),
            unread_count_requests: BTreeSet::new(),
            random_id_map: FxHashMap::default(),
            typing_actions: FxHashMap::default(),
            main_list: MainList::new(0, DEFAULT_MAIN_PINNED_LIMIT),
            archive: Folder::new(ARCHIVE_FOLDER_ID, DEFAULT_FOLDER_PINNED_LIMIT),
            filters: ChatFilters::new(DEFAULT_FOLDER_PINNED_LIMIT),
            forums: FxHashMap::default(),
            saved_messages: SavedMessagesState::new(None),
            monoforums: FxHashMap::default(),
            saved_reaction_tags: SavedReactionTagsState::default(),
            scheduled_messages: ScheduledMessagesStore::default(),
            shortcut_messages: ShortcutMessagesStore::default(),
            sort_shift: 0,
            changes: Changes::new(broadcast_capacity),
            peer_message_ids: FxHashMap::default(),
            media_messages: FxHashMap::default(),
            message_media: FxHashMap::default(),
            media_dirty: FxHashSet::default(),
            peers_generation: 0,
            username_index: Mutex::new(UsernameIndex::default()),
            message_budget: Some(DEFAULT_MESSAGE_BUDGET),
            next_unload_check: DEFAULT_MESSAGE_BUDGET,
            history_touch: FxHashMap::default(),
            touch_tick: 0,
            open_histories: FxHashSet::default(),
            notify_defaults: [None, None, None],
            notify_known: FxHashSet::default(),
            unload_armed: false,
        }
    }

    /// Returns the logged-in account's `UserId`.
    #[inline]
    pub const fn self_user_id(&self) -> UserId {
        self.self_user_id
    }

    /// Returns the logged-in account's `PeerId`.
    #[inline]
    pub const fn self_peer_id(&self) -> PeerId {
        PeerId::from_user(self.self_user_id)
    }

    /// Subscribes to change notifications (`tokio::sync::broadcast::Receiver<ChangeEvent>`).
    pub fn subscribe(&self) -> broadcast::Receiver<ChangeEvent> {
        self.changes.subscribe()
    }

    /// Returns direct access to the `Changes` notification hub.
    #[inline]
    pub fn changes(&self) -> &Changes {
        &self.changes
    }

    /// Returns current account send-permission context (frozen, premium, test env).
    #[inline]
    pub const fn send_context(&self) -> SendCheckContext {
        self.send_context
    }

    /// Updates account send-permission context.
    #[inline]
    pub fn set_send_context(&mut self, ctx: SendCheckContext) {
        self.send_context = ctx;
    }

    /// Returns global unread badge settings.
    #[inline]
    pub const fn badge_settings(&self) -> BadgeSettings {
        self.badge_settings
    }

    /// Updates global unread badge settings.
    #[inline]
    pub fn set_badge_settings(&mut self, settings: BadgeSettings) {
        self.badge_settings = settings;
    }

    /// Returns whether unmuted unread chats are sorted above read chats.
    #[inline]
    pub const fn dialogs_unread_on_top(&self) -> bool {
        self.dialogs_unread_on_top
    }

    /// Enables or disables "keep unmuted unread chats on top" sorting and re-sorts all lists.
    pub fn set_dialogs_unread_on_top(&mut self, enabled: bool) {
        if self.dialogs_unread_on_top != enabled {
            self.dialogs_unread_on_top = enabled;
            let peer_ids: Vec<PeerId> = self.histories.keys().copied().collect();
            for peer_id in peer_ids {
                self.sync_dialog_lists(peer_id);
            }
        }
    }

    // -------------------------------------------------------------------------
    // Peers
    // -------------------------------------------------------------------------

    /// Inserts or updates a `Peer`, broadcasting `PeerUpdated` (and `NameUpdated` on rename).
    pub fn upsert_peer(&mut self, peer: Peer, update_flags: PeerUpdateFlags) {
        self.peers_changed();
        let peer_id = peer.id();
        let hash = match &peer {
            Peer::User(u) if u.access_hash != 0 => Some(u.access_hash as i64),
            Peer::Channel(ch) if ch.access_hash != 0 => Some(ch.access_hash as i64),
            _ => None,
        };
        if let Some(h) = hash {
            self.access_hashes.insert(peer_id, h);
        }
        let old_letters: Option<Vec<char>> = self.peers.get(&peer_id).and_then(|prev| {
            if prev.name() != peer.name() {
                Some(prev.common().name_first_letters.iter().copied().collect())
            } else {
                None
            }
        });
        self.peers.insert(peer_id, peer);
        if let Some(letters) = old_letters {
            self.changes.name_updated(peer_id, letters);
        }
        self.changes.peer_updated(peer_id, update_flags);
        if self.histories.contains_key(&peer_id) {
            self.sync_dialog_lists(peer_id);
        }
    }

    /// Records a peer's MTProto `access_hash` for building `InputPeer`/`InputUser`/`InputChannel`.
    ///
    /// Also updates the loaded `User`/`Channel` struct if present. Ignores `0` if a non-zero
    /// access hash is already known for this peer (matching tdesktop's `min` peer handling).
    pub fn set_peer_access_hash(&mut self, peer_id: PeerId, access_hash: i64) {
        if access_hash == 0 && self.access_hashes.contains_key(&peer_id) {
            return;
        }
        if access_hash != 0 {
            self.access_hashes.insert(peer_id, access_hash);
        }
        match self.peers.get_mut(&peer_id) {
            Some(Peer::User(u)) if access_hash != 0 || u.access_hash == 0 => {
                u.set_access_hash(access_hash as u64);
            }
            Some(Peer::Channel(ch)) if access_hash != 0 || ch.access_hash == 0 => {
                ch.access_hash = access_hash as u64;
            }
            _ => {}
        }
    }

    /// Returns the stored MTProto `access_hash` for `peer_id`, if known.
    #[inline]
    pub fn peer_access_hash(&self, peer_id: PeerId) -> Option<i64> {
        if let Some(&h) = self.access_hashes.get(&peer_id) {
            return Some(h);
        }
        match self.peers.get(&peer_id) {
            Some(Peer::User(u)) if u.access_hash != 0 => Some(u.access_hash as i64),
            Some(Peer::Channel(ch)) if ch.access_hash != 0 => Some(ch.access_hash as i64),
            _ => None,
        }
    }

    /// Returns a mutable reference to a loaded `User` by `UserId`.
    #[inline]
    pub fn user_mut(&mut self, id: UserId) -> Option<&mut User> {
        self.peers_changed();
        self.peers
            .get_mut(&PeerId::from_user(id))
            .and_then(Peer::as_user_mut)
    }

    /// Returns a mutable reference to a loaded `Chat` by `ChatId`.
    #[inline]
    pub fn chat_mut(&mut self, id: ChatId) -> Option<&mut Chat> {
        self.peers_changed();
        self.peers
            .get_mut(&PeerId::from_chat(id))
            .and_then(Peer::as_chat_mut)
    }

    /// Returns a mutable reference to a loaded `Channel` by `ChannelId`.
    #[inline]
    pub fn channel_mut(&mut self, id: ChannelId) -> Option<&mut Channel> {
        self.peers_changed();
        self.peers
            .get_mut(&PeerId::from_channel(id))
            .and_then(Peer::as_channel_mut)
    }

    /// Ensures a `User` entry exists for `id` and returns a mutable reference to it.
    pub fn ensure_user(&mut self, id: UserId) -> &mut User {
        self.peers_changed();
        let is_self = id == self.self_user_id;
        let peer_id = PeerId::from_user(id);
        let entry = self
            .peers
            .entry(peer_id)
            .or_insert_with(|| Peer::User(Box::new(User::new(id, is_self))));
        match entry {
            Peer::User(u) => u,
            _ => unreachable!("PeerId::from_user always maps to Peer::User"),
        }
    }

    /// Ensures a `Chat` entry exists for `id` and returns a mutable reference to it.
    pub fn ensure_chat(&mut self, id: ChatId) -> &mut Chat {
        self.peers_changed();
        let peer_id = PeerId::from_chat(id);
        let entry = self
            .peers
            .entry(peer_id)
            .or_insert_with(|| Peer::Chat(Box::new(Chat::new(id))));
        match entry {
            Peer::Chat(c) => c,
            _ => unreachable!("PeerId::from_chat always maps to Peer::Chat"),
        }
    }

    /// Ensures a `Channel` entry exists for `id` and returns a mutable reference to it.
    pub fn ensure_channel(&mut self, id: ChannelId) -> &mut Channel {
        self.peers_changed();
        let peer_id = PeerId::from_channel(id);
        let entry = self
            .peers
            .entry(peer_id)
            .or_insert_with(|| Peer::Channel(Box::new(Channel::new(id))));
        match entry {
            Peer::Channel(ch) => ch,
            _ => unreachable!("PeerId::from_channel always maps to Peer::Channel"),
        }
    }

    /// Updates a user's online/last-seen status (`updateUserStatus`) and emits
    /// `PeerUpdateFlags::ONLINE_STATUS` if it changed.
    ///
    /// Matches `Updates::applyUpdateNoPtsCheck` (`api/api_updates.cpp:2133-2142`):
    /// only updates already-loaded (`LoadedStatus::Normal`/`Full`) users and never
    /// creates phantom entries for unknown user IDs.
    pub fn update_user_status(&mut self, user_id: UserId, status: UserStatusInput) -> bool {
        if !self.user_loaded(user_id) {
            return false;
        }
        let peer_id = PeerId::from_user(user_id);
        let changed = {
            let Some(user) = self.peers.get_mut(&peer_id).and_then(Peer::as_user_mut) else {
                return false;
            };
            let next = LastseenStatus::from_api(status, user.lastseen);
            if user.lastseen != next {
                user.lastseen = next;
                true
            } else {
                false
            }
        };
        if changed {
            self.changes
                .peer_updated(peer_id, PeerUpdateFlags::ONLINE_STATUS);
        }
        changed
    }

    /// Stores or refreshes full `PhotoData` (including `access_hash` and `file_reference`)
    /// in the session's photo registry (`Data::Session::processPhoto` in `data/data_session.cpp`).
    pub fn cache_photo(&mut self, photo: PhotoData) {
        if photo.id.is_zero() || photo.id == UNKNOWN_PHOTO_ID {
            return;
        }
        self.photos.insert(photo.id, photo);
    }

    /// Looks up cached [`PhotoData`] by `PhotoId` (`Data::Session::photo` in `data/data_session.cpp`).
    #[must_use]
    pub fn photo(&self, id: PhotoId) -> Option<&PhotoData> {
        if id.is_zero() || id == UNKNOWN_PHOTO_ID {
            return None;
        }
        self.photos.get(&id)
    }

    /// Removes a deleted photo from the session's photo cache.
    pub fn remove_photo(&mut self, id: PhotoId) -> Option<PhotoData> {
        self.photos.remove(&id)
    }

    /// Returns the cached [`PhotoData`] for `peer_id`'s currently active profile/chat photo,
    /// if both the peer's `userpic_photo_id` and its full photo metadata are known.
    #[must_use]
    pub fn peer_active_photo(&self, peer_id: PeerId) -> Option<&PhotoData> {
        let photo_id = self.peer(peer_id)?.common().userpic_photo_id;
        self.photo(photo_id)
    }

    /// Sets a loaded peer's blocked status (`PeerData::setIsBlocked` in
    /// `data/data_peer.cpp:2113-2130`, `api/api_updates.cpp:2270-2275`).
    pub fn set_peer_blocked(&mut self, peer_id: PeerId, blocked: bool) -> bool {
        if !self.peer_loaded(peer_id) {
            return false;
        }
        let changed = self
            .peers
            .get_mut(&peer_id)
            .is_some_and(|p| p.set_is_blocked(blocked));
        if changed {
            self.changes
                .peer_updated(peer_id, PeerUpdateFlags::IS_BLOCKED);
        }
        changed
    }

    /// Looks up a loaded peer by username (case-insensitive, stripping optional `@` and
    /// whitespace), matching `Data::Session::peerByUsername` (`data/data_session.cpp:1675-1687`).
    #[must_use]
    pub fn peer_by_username(&self, username: &str) -> Option<PeerId> {
        let trimmed = username.trim();
        let needle = trimmed.strip_prefix('@').unwrap_or(trimmed);
        if needle.is_empty() {
            return None;
        }
        let mut index = self
            .username_index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if index.generation != Some(self.peers_generation) {
            index.by_name.clear();
            for (&peer_id, peer) in &self.peers {
                if !peer.is_loaded() {
                    continue;
                }
                for username in peer.usernames() {
                    index
                        .by_name
                        .entry(username.to_ascii_lowercase())
                        .or_insert(peer_id);
                }
            }
            index.generation = Some(self.peers_generation);
        }
        index.by_name.get(&needle.to_ascii_lowercase()).copied()
    }

    /// Marks the peers as possibly changed for [`Self::peer_by_username`].
    #[inline]
    fn peers_changed(&mut self) {
        self.peers_generation = self.peers_generation.wrapping_add(1);
    }

    /// Records that `full_id` references `peer_id` when `peer_id` is not yet fully loaded
    /// (`Data::Session::fillMessagePeer`, `data/data_session.cpp:6145-6149`).
    pub fn record_message_peer(&mut self, full_id: FullMsgId, peer_id: PeerId) {
        if peer_id.is_zero()
            || peer_id == self.self_peer_id()
            || !full_id.msg.is_server()
            || full_id.peer == peer_id
            || self.peer_loaded(peer_id)
        {
            return;
        }
        let list = self.messages_with_peer.entry(peer_id).or_default();
        if !list.contains(&full_id) {
            list.push(full_id);
        }
    }

    /// Returns a live server message referencing `peer_id` (from another loaded chat/channel)
    /// for building `inputPeerUserFromMessage` / `inputPeerChannelFromMessage`
    /// (`Data::Session::messageWithPeer`, `data/data_session.cpp:6265-6282`).
    #[must_use]
    pub fn message_with_peer(&self, peer_id: PeerId) -> Option<FullMsgId> {
        if peer_id == self.self_peer_id() {
            return None;
        }
        let list = self.messages_with_peer.get(&peer_id)?;
        list.iter()
            .copied()
            .find(|fid| fid.peer != peer_id && self.messages.contains_key(fid))
    }

    /// Returns whether the account's contacts list has been loaded (`Session::contactsLoaded`).
    #[inline]
    #[must_use]
    pub const fn contacts_loaded(&self) -> bool {
        self.contacts_loaded
    }

    /// Marks whether the account's contacts list has been loaded.
    #[inline]
    pub fn set_contacts_loaded(&mut self, loaded: bool) {
        self.contacts_loaded = loaded;
    }

    /// Returns sorted bare user IDs of all loaded contact users (used for computing
    /// `contacts.getContacts` 64-bit hashes).
    #[must_use]
    pub fn contact_user_ids(&self) -> Vec<u64> {
        let mut ids: Vec<u64> = self
            .peers
            .values()
            .filter_map(Peer::as_user)
            .filter(|u| u.is_contact())
            .map(|u| u.user_id().0)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    fn record_message_peers(&mut self, msg: &Message) {
        let full_id = msg.full_id();
        if !full_id.msg.is_server() {
            return;
        }
        if let Some(from) = msg.from_id {
            self.record_message_peer(full_id, from);
        }
        if let Some(bot_id) = msg.via_bot_id {
            self.record_message_peer(full_id, PeerId::from_user(bot_id));
        }
        if let Some(fwd) = &msg.forward_from
            && let Some(from) = fwd.from_id
        {
            self.record_message_peer(full_id, from);
        }
        match &msg.content {
            MessageContent::Regular { text, .. } => {
                for entity in &text.entities {
                    if let EntityKind::MentionName { user_id } = entity.kind {
                        self.record_message_peer(full_id, PeerId::from_user(user_id));
                    }
                }
            }
            MessageContent::Service(action) => match action.as_ref() {
                ServiceAction::ChatAddUser { users } | ServiceAction::ChatCreate { users, .. } => {
                    for &uid in users {
                        self.record_message_peer(full_id, PeerId::from_user(uid));
                    }
                }
                ServiceAction::ChatJoinedByLink { inviter_id } => {
                    self.record_message_peer(full_id, PeerId::from_user(*inviter_id));
                }
                ServiceAction::ChatDeleteUser { user_id } => {
                    self.record_message_peer(full_id, PeerId::from_user(*user_id));
                }
                _ => {}
            },
            MessageContent::Empty => {}
        }
    }

    /// Records a typing/activity action in `peer_id` from `from_id` and broadcasts
    /// [`ChangeEvent::UserTyping`].
    pub fn set_typing_action(
        &mut self,
        peer_id: PeerId,
        from_id: PeerId,
        top_msg_id: Option<MsgId>,
        action: TypingAction,
    ) {
        let key = (peer_id, from_id, top_msg_id);
        if action == TypingAction::Cancel {
            self.typing_actions.remove(&key);
        } else {
            self.typing_actions.insert(key, action);
        }
        self.changes
            .user_typing(peer_id, from_id, top_msg_id, action);
    }

    /// Returns currently active typing actions in `peer_id`.
    pub fn active_typing(&self, peer_id: PeerId) -> Vec<(PeerId, Option<MsgId>, TypingAction)> {
        self.typing_actions
            .iter()
            .filter_map(|(&(p, from_id, top_msg_id), &action)| {
                (p == peer_id).then_some((from_id, top_msg_id, action))
            })
            .collect()
    }

    /// Inserts or updates a `User`.
    pub fn upsert_user(&mut self, user: User, update_flags: PeerUpdateFlags) {
        self.upsert_peer(Peer::User(Box::new(user)), update_flags);
    }

    /// Inserts or updates a basic `Chat`.
    pub fn upsert_chat(&mut self, chat: Chat, update_flags: PeerUpdateFlags) {
        self.upsert_peer(Peer::Chat(Box::new(chat)), update_flags);
    }

    /// Inserts or updates a `Channel`.
    pub fn upsert_channel(&mut self, channel: Channel, update_flags: PeerUpdateFlags) {
        self.upsert_peer(Peer::Channel(Box::new(channel)), update_flags);
    }

    /// Returns a reference to a loaded peer by `PeerId`.
    #[inline]
    pub fn peer(&self, id: PeerId) -> Option<&Peer> {
        self.peers.get(&id)
    }

    /// Returns a mutable reference to a loaded peer by `PeerId`.
    #[inline]
    pub fn peer_mut(&mut self, id: PeerId) -> Option<&mut Peer> {
        self.peers_changed();
        self.peers.get_mut(&id)
    }

    /// Returns `true` if a peer exists in the registry with full non-`min` metadata
    /// (`Data::Session::peerLoaded`, requiring `LoadedStatus::Normal` or `LoadedStatus::Full`).
    #[inline]
    pub fn peer_loaded(&self, id: PeerId) -> bool {
        self.peers.get(&id).is_some_and(Peer::is_loaded)
    }

    /// Returns `true` if a peer exists in the registry with at least `min` metadata
    /// (`PeerData::isMinimalLoaded`).
    #[inline]
    pub fn peer_minimal_loaded(&self, id: PeerId) -> bool {
        self.peers.get(&id).is_some_and(Peer::is_minimal_loaded)
    }

    /// Returns `true` if a user is loaded with non-`min` metadata (`Data::Session::userLoaded`).
    #[inline]
    pub fn user_loaded(&self, id: UserId) -> bool {
        self.peer_loaded(PeerId::from_user(id))
    }

    /// Returns `true` if a basic chat is loaded (`Data::Session::chatLoaded`).
    #[inline]
    pub fn chat_loaded(&self, id: ChatId) -> bool {
        self.peer_loaded(PeerId::from_chat(id))
    }

    /// Returns `true` if a channel is loaded with non-`min` metadata (`Data::Session::channelLoaded`).
    #[inline]
    pub fn channel_loaded(&self, id: ChannelId) -> bool {
        self.peer_loaded(PeerId::from_channel(id))
    }

    /// Returns a loaded `User` by `UserId`.
    #[inline]
    pub fn user(&self, id: UserId) -> Option<&User> {
        self.peers
            .get(&PeerId::from_user(id))
            .and_then(Peer::as_user)
    }

    /// Returns a loaded `Chat` by `ChatId`.
    #[inline]
    pub fn chat(&self, id: ChatId) -> Option<&Chat> {
        self.peers
            .get(&PeerId::from_chat(id))
            .and_then(Peer::as_chat)
    }

    /// Returns a loaded `Channel` by `ChannelId`.
    #[inline]
    pub fn channel(&self, id: ChannelId) -> Option<&Channel> {
        self.peers
            .get(&PeerId::from_channel(id))
            .and_then(Peer::as_channel)
    }

    /// Links a basic group chat to its migrated megagroup channel (`Data::ApplyMigration`).
    ///
    /// Sets `Chat::migrated_to`, marks `ChatFlags::DEACTIVATED`, ensures
    /// `ChannelFlags::MEGAGROUP` and `MegagroupInfo::migrated_from`, and emits
    /// `PeerUpdateFlags::MIGRATION`.
    pub fn apply_migration(&mut self, chat_id: ChatId, channel_id: ChannelId) {
        self.peers_changed();
        let chat_peer_id = PeerId::from_chat(chat_id);
        let channel_peer_id = PeerId::from_channel(channel_id);

        let chat_entry = self
            .peers
            .entry(chat_peer_id)
            .or_insert_with(|| Peer::Chat(Box::new(Chat::new(chat_id))));
        if let Some(chat) = chat_entry.as_chat_mut() {
            chat.migrated_to = Some(channel_id);
            chat.flags.insert(ChatFlags::DEACTIVATED);
        }

        let channel_entry = self
            .peers
            .entry(channel_peer_id)
            .or_insert_with(|| Peer::Channel(Box::new(Channel::new(channel_id))));
        let channel_am_in = if let Some(channel) = channel_entry.as_channel_mut() {
            channel.flags.insert(ChannelFlags::MEGAGROUP);
            channel.ensure_mg_info().migrated_from = Some(chat_id);
            channel.am_in()
        } else {
            false
        };

        if channel_am_in {
            // Hide the migrated basic group from dialog lists once the user is in the
            // destination megagroup (`data/data_session.cpp:2873-2881`, `Session::applyDialog`).
            if let Some(chat_history) = self.histories.get_mut(&chat_peer_id)
                && chat_history.in_chat_list
            {
                chat_history.in_chat_list = false;
                self.sync_dialog_lists(chat_peer_id);
            }
            self.changes
                .peer_updated(chat_peer_id, PeerUpdateFlags::MIGRATION);
        }
        self.changes
            .peer_updated(channel_peer_id, PeerUpdateFlags::MIGRATION);
    }

    /// Resolves a migrated basic group `PeerId` to its target megagroup `PeerId`
    /// if the current account is a member of the target megagroup (`PeerData::migrateToOrMe`).
    pub fn migrate_to_or_self(&self, peer_id: PeerId) -> PeerId {
        if let Some(chat) = self.peers.get(&peer_id).and_then(Peer::as_chat)
            && let Some(channel_id) = chat.migrated_to
        {
            let target_id = PeerId::from_channel(channel_id);
            if self
                .peers
                .get(&target_id)
                .and_then(Peer::as_channel)
                .is_some_and(Channel::am_in)
            {
                return target_id;
            }
        }
        peer_id
    }

    /// Checks `CanSendAnyOf` for `peer_id` using this session's `SendCheckContext`.
    pub fn can_send_any_of(
        &self,
        peer_id: PeerId,
        rights: ChatBannedRights,
        forbid_in_forums: bool,
    ) -> bool {
        self.peers
            .get(&peer_id)
            .is_some_and(|p| p.can_send_any_of(rights, forbid_in_forums, self.send_context))
    }

    /// Checks whether the current account can write to `peer_id`.
    pub fn can_write(&self, peer_id: PeerId) -> bool {
        self.peers
            .get(&peer_id)
            .is_some_and(|p| p.can_write(self.send_context))
    }

    // -------------------------------------------------------------------------
    // Messages
    // -------------------------------------------------------------------------

    /// Inserts a message into the session, updates its owning `History`'s loaded
    /// ranges and top message, refreshes dialog lists, and broadcasts notifications.
    ///
    /// Used for messages that come with history slices, dialogs pages and other loads
    /// (`NewMessageType::Existing` / `Last`): they never raise desktop notifications and never
    /// change the chat's unread count, which comes with the dialog.
    pub fn insert_message(&mut self, message: Message) {
        self.insert_message_typed(message, false);
    }

    /// Inserts a message that arrived as a live update (`updateNewMessage`,
    /// `updateShortMessage`, a `getDifference` during the session): like
    /// [`Self::insert_message`], but a newly added message is also flagged
    /// [`MessageUpdateFlags::NEW_UNREAD`] (`NewMessageType::Unread`) so the notifications
    /// layer may show it, and an incoming one past the read mark raises the unread count
    /// (`data/data_session.cpp:3629-3661`, `history/history.cpp:674, 1717-1723`).
    ///
    /// A new service message that changes a loaded checklist or poll is applied to it
    /// (`History::applyServiceChanges`).
    pub fn insert_new_unread_message(&mut self, message: Message) {
        self.insert_new_message_typed(message, true);
    }

    /// Inserts a message that arrived as a live update and applies its service changes
    /// when it is new (`History::addNewMessage`, `history/history.cpp:674-705`).
    fn insert_new_message_typed(&mut self, message: Message, unread_type: bool) {
        if !self.messages.contains_key(&message.full_id()) {
            self.apply_service_changes(&message);
        }
        self.insert_message_typed(message, unread_type);
    }

    /// Stores a message loaded as part of a page that is not the chat's own bottom:
    /// topic and comment pages, search results, unread-mention pages, refreshed references
    /// (`NewMessageType::Existing`, `History::addNewMessage` returns right after `createItem`,
    /// `history/history.cpp:669-700`).
    ///
    /// A message seen for the first time is only stored: no "new message" event, no unread,
    /// mention or reaction counter change, no loaded-range, top-message or chat-list change.
    /// A message the session already had is replaced and announced as edited, like
    /// [`Self::insert_message`] does, so open views repaint it.
    pub fn insert_existing_message(&mut self, message: Message) {
        let full_id = message.full_id();
        if self.messages.contains_key(&full_id) {
            self.insert_message_typed(message, false);
            return;
        }
        if !full_id.peer.is_channel() && full_id.msg.is_server() {
            self.non_channel_messages.insert(full_id.msg, full_id.peer);
        }
        self.record_message_peers(&message);
        self.store_message(message);
        self.ensure_history(full_id.peer);
        self.maybe_unload(full_id.peer);
    }

    fn insert_message_typed(&mut self, message: Message, unread_type: bool) {
        let full_id = message.full_id();
        let peer_id = message.peer_id;
        let msg_id = message.id;
        let date = message.date;
        let is_regular = message.is_regular();
        let is_outgoing = message.is_outgoing();
        let is_post = message.is_post();
        let mentions_me = message.mentions_me();
        let has_unread_reaction = message.has_unread_reaction();

        let is_new = !self.messages.contains_key(&full_id);
        self.record_message_peers(&message);
        self.store_message(message);
        if !peer_id.is_channel() && msg_id.is_server() {
            self.non_channel_messages.insert(msg_id, peer_id);
        }

        let history = self
            .histories
            .entry(peer_id)
            .or_insert_with(|| History::new(peer_id));
        if is_regular {
            history.loaded_ranges.add_new(msg_id);
        }
        if history.top_message_id.is_none_or(|top| msg_id >= top) {
            history.top_message_id = Some(msg_id);
            if date > history.chat_list_time_id {
                history.chat_list_time_id = date;
            }
        }
        // Keep unread mention/reaction counters in sync with inserted messages
        // (`history/history_unread_things.cpp:108-133`, `HistoryUnreadThings::Proxy::add`).
        if is_regular && mentions_me && history.unread_mentions.insert(msg_id) {
            history.unread_mentions_count += 1;
            self.changes
                .history_updated(peer_id, HistoryUpdateFlags::UNREAD_MENTIONS);
        }
        if is_regular && has_unread_reaction && history.unread_reactions.insert(msg_id) {
            history.unread_reactions_count += 1;
            self.changes
                .history_updated(peer_id, HistoryUpdateFlags::UNREAD_REACTIONS);
        }
        // Increment known unread count for newly arriving unread incoming messages
        // (`History::newItemAdded`, `history/history.cpp:1717-1723`). Only a message added as
        // `NewMessageType::Unread` gets there (`History::addNewItem` / `addNewToBack`,
        // `history.cpp:865-900, 1210-1214`): one stored with a dialogs page or the startup
        // catch-up is already in the server's `dialog.unread_count`.
        if unread_type
            && is_new
            && is_regular
            && (!is_outgoing || is_post)
            && msg_id > history.inbox_read_till_id()
            && let Some(count) = history.unread_count.as_mut()
        {
            *count += 1;
            self.changes
                .history_updated(peer_id, HistoryUpdateFlags::UNREAD_VIEW);
        }
        history.in_chat_list = true;

        self.changes.message_updated(
            full_id,
            if is_new && unread_type {
                MessageUpdateFlags::NEW_ADDED
                    | MessageUpdateFlags::NEW_UNREAD
                    | MessageUpdateFlags::DIALOG_ROW_REFRESH
            } else if is_new {
                MessageUpdateFlags::NEW_ADDED | MessageUpdateFlags::DIALOG_ROW_REFRESH
            } else {
                MessageUpdateFlags::EDITED | MessageUpdateFlags::DIALOG_ROW_REPAINT
            },
        );
        self.sync_dialog_lists(peer_id);
        self.maybe_unload(peer_id);
    }

    /// Merges a server-confirmed `updateNewMessage` into an already-promoted local pending
    /// message if one exists at `message.full_id()`, otherwise inserts it normally
    /// (`data/data_session.cpp:3072-3087`, `Session::updateExistingMessage`;
    /// `history/history_item.cpp:2849-2944`, `HistoryItem::applySentMessage`).
    pub fn apply_sent_or_new_message(&mut self, message: Message) {
        self.apply_sent_or_new_message_typed(message, true);
    }

    /// Like [`Self::apply_sent_or_new_message`]; `unread_type == false` adds a new message
    /// like history (`NewMessageType::Existing`), so it never raises a notification or the
    /// unread count.
    pub fn apply_sent_or_new_message_typed(&mut self, mut message: Message, unread_type: bool) {
        let full_id = message.full_id();
        if let Some(existing) = self.messages.get(&full_id)
            && existing
                .flags
                .intersects(MessageFlags::BEING_SENT | MessageFlags::SENDING_FAILED)
        {
            message
                .flags
                .remove(MessageFlags::BEING_SENT | MessageFlags::SENDING_FAILED);
            self.insert_message(message);
            return;
        }
        let peer_id = message.peer_id;
        self.insert_new_message_typed(message, unread_type);
        self.request_dialog_entry_if_folder_unknown(peer_id);
    }

    /// Queues `peer_id`'s dialog entry for `messages.getPeerDialogs` when a new message arrived
    /// but the history's folder is not known yet (`History::addNewItem` ->
    /// `Histories::requestDialogEntry`, `history/history.cpp:1731-1733`). Until the entry
    /// arrives the history stays out of every chat list (`History::shouldBeInChatList`).
    pub fn request_dialog_entry_if_folder_unknown(&mut self, peer_id: PeerId) {
        let unknown = self
            .histories
            .get(&peer_id)
            .is_some_and(|h| !h.folder_known());
        let community = self
            .peers
            .get(&peer_id)
            .and_then(Peer::as_channel)
            .is_some_and(Channel::is_community);
        if unknown && !community {
            self.dialog_entry_requests.insert(peer_id);
        }
    }

    /// Histories that belong in a chat list but stay out of every list until their folder is
    /// known ([`Self::request_dialog_entry_if_folder_unknown`]), sorted by peer.
    #[must_use]
    pub fn histories_awaiting_folder(&self) -> Vec<PeerId> {
        let mut out: Vec<PeerId> = self
            .histories
            .values()
            .filter(|h| h.in_chat_list && !h.folder_known())
            .map(|h| h.peer_id)
            .collect();
        out.sort_unstable();
        out
    }

    /// `true` when [`Self::take_dialog_entry_requests`] has peers to return.
    #[must_use]
    pub fn has_dialog_entry_requests(&self) -> bool {
        !self.dialog_entry_requests.is_empty() || !self.unread_count_requests.is_empty()
    }

    /// Takes the queued dialog entry requests: histories whose folder is still unknown and
    /// histories whose unread count could not be counted after a read, sorted by peer.
    pub fn take_dialog_entry_requests(&mut self) -> Vec<PeerId> {
        let mut out = std::mem::take(&mut self.unread_count_requests);
        out.extend(
            std::mem::take(&mut self.dialog_entry_requests)
                .into_iter()
                .filter(|pid| self.histories.get(pid).is_some_and(|h| !h.folder_known())),
        );
        out.into_iter().collect()
    }

    /// Registers a `random_id -> FullMsgId` mapping for an outgoing local message.
    pub fn register_random_id(&mut self, random_id: i64, full_id: FullMsgId) {
        self.random_id_map.insert(random_id, full_id);
    }

    /// Looks up a message ID by outgoing `random_id`.
    #[inline]
    pub fn message_id_by_random_id(&self, random_id: i64) -> Option<FullMsgId> {
        self.random_id_map.get(&random_id).copied()
    }

    /// Removes a `random_id` mapping without resolving it (e.g. when sending fails).
    pub fn unregister_random_id(&mut self, random_id: i64) -> Option<FullMsgId> {
        self.random_id_map.remove(&random_id)
    }

    /// Resolves an `updateMessageID` mapping `random_id -> server_id`, re-keying any stored
    /// local pending message to `server_id`, updating history loaded ranges, and notifying.
    ///
    /// If the server message already arrived at `server_id` before `updateMessageID`, removes
    /// the stale local placeholder instead of overwriting the server message
    /// (`api/api_updates.cpp:1721-1745`).
    pub fn resolve_random_id(&mut self, random_id: i64, server_id: MsgId) -> Option<FullMsgId> {
        let local_id = self.random_id_map.remove(&random_id)?;
        let peer_id = local_id.peer;
        let new_full_id = FullMsgId::new(peer_id, server_id);
        if let Some(mut msg) = self.take_message(local_id) {
            if self.messages.contains_key(&new_full_id) {
                // Server message already arrived (`updateNewMessage` before `updateMessageID`):
                // preserve the authoritative server message and drop the local placeholder.
                if let Some(history) = self.histories.get_mut(&peer_id) {
                    history.loaded_ranges.remove_one(local_id.msg, true);
                    if history.top_message_id == Some(local_id.msg) {
                        history.top_message_id = Some(server_id);
                    }
                }
                self.changes
                    .message_updated(local_id, MessageUpdateFlags::DESTROYED);
                self.changes.message_updated(
                    new_full_id,
                    MessageUpdateFlags::EDITED | MessageUpdateFlags::DIALOG_ROW_REFRESH,
                );
                self.sync_dialog_lists(peer_id);
                return Some(local_id);
            }
            msg.id = server_id;
            msg.flags
                .remove(MessageFlags::BEING_SENT | MessageFlags::SENDING_FAILED);
            let date = msg.date;
            self.store_message(msg);
            if !peer_id.is_channel() && server_id.is_server() {
                self.non_channel_messages.insert(server_id, peer_id);
            }
            if let Some(history) = self.histories.get_mut(&peer_id) {
                history.loaded_ranges.remove_one(local_id.msg, true);
                history.loaded_ranges.add_new(server_id);
                if history.top_message_id == Some(local_id.msg)
                    || history.top_message_id.is_none_or(|top| server_id >= top)
                {
                    history.top_message_id = Some(server_id);
                    if date > history.chat_list_time_id {
                        history.chat_list_time_id = date;
                    }
                }
            }
            self.changes.message_updated(
                new_full_id,
                MessageUpdateFlags::EDITED | MessageUpdateFlags::DIALOG_ROW_REFRESH,
            );
            self.sync_dialog_lists(peer_id);
        }
        Some(local_id)
    }

    /// Inserts a loaded history slice into `Session`, merging `no_skip_range` and `count`
    /// into the peer's `SparseIdsList`.
    pub fn add_history_slice(
        &mut self,
        peer_id: PeerId,
        messages: Vec<Message>,
        no_skip_range: MsgRange,
        count: Option<i32>,
    ) {
        let mut ids = Vec::with_capacity(messages.len());
        let mut newest: Option<(MsgId, crate::id::TimeId)> = None;
        for msg in messages {
            let msg_id = msg.id;
            let date = msg.date;
            if msg.is_regular() {
                ids.push(msg_id);
            }
            if newest.is_none_or(|(best, _)| msg_id > best) {
                newest = Some((msg_id, date));
            }
            if !peer_id.is_channel() && msg_id.is_server() {
                self.non_channel_messages.insert(msg_id, peer_id);
            }
            self.record_message_peers(&msg);
            self.store_message(msg);
        }
        let history = self.ensure_history(peer_id);
        history.loaded_ranges.add_slice(ids, no_skip_range, count);
        if let Some((top_id, date)) = newest
            && history.top_message_id.is_none_or(|cur| top_id >= cur)
        {
            history.top_message_id = Some(top_id);
            if date > history.chat_list_time_id {
                history.chat_list_time_id = date;
            }
        }
        self.touch_history(peer_id);
        self.maybe_unload(peer_id);
    }

    /// Marks the bottom of `peer_id`'s history as needing reload (e.g. on
    /// `updates.channelDifferenceTooLong`).
    pub fn invalidate_history_bottom(&mut self, peer_id: PeerId) {
        if let Some(history) = self.histories.get_mut(&peer_id) {
            history.loaded_ranges.invalidate_bottom();
        }
    }

    /// Marks media and mention contents as read for `ids` (`updateReadMessagesContents` /
    /// `updateChannelReadMessagesContents`).
    ///
    /// Pass `peer_id = Some(channel_peer)` for channel messages or `None` for non-channel
    /// messages. Returns IDs not currently loaded in memory.
    pub fn mark_messages_contents_read(
        &mut self,
        peer_id: Option<PeerId>,
        ids: &[MsgId],
    ) -> Vec<MsgId> {
        let mut unknown = Vec::new();
        for &msg_id in ids {
            let resolved_peer = match peer_id {
                Some(p) => Some(p),
                None => self.non_channel_messages.get(&msg_id).copied(),
            };
            let Some(p) = resolved_peer else {
                unknown.push(msg_id);
                continue;
            };
            let full_id = FullMsgId::new(p, msg_id);
            let Some(msg) = self.messages.get_mut(&full_id) else {
                unknown.push(msg_id);
                continue;
            };
            let had_media_unread = msg.flags.contains(MessageFlags::MEDIA_IS_UNREAD);
            let had_mention = msg.flags.contains(MessageFlags::MENTIONS_ME);
            let had_reaction = msg.flags.contains(MessageFlags::HAS_UNREAD_REACTION);
            if had_media_unread || had_mention || had_reaction {
                // Match `HistoryItem::markContentsRead` (`history/history_item.cpp:1939-2013`)
                // and `HistoryUnreadThings::Proxy::erase` (`history/history_unread_things.cpp:135-145`).
                msg.flags.remove(
                    MessageFlags::MEDIA_IS_UNREAD
                        | MessageFlags::MENTIONS_ME
                        | MessageFlags::HAS_UNREAD_REACTION,
                );
                if had_reaction && let Some(reactions) = msg.reactions.as_mut() {
                    for rr in &mut reactions.recent_reactors {
                        rr.unread = false;
                    }
                }
                let mut unread_changed = false;
                if let Some(history) = self.histories.get_mut(&p) {
                    if had_mention {
                        history.unread_mentions.remove(&msg_id);
                        history.unread_mentions_count = (history.unread_mentions_count - 1).max(0);
                        self.changes
                            .history_updated(p, HistoryUpdateFlags::UNREAD_MENTIONS);
                        unread_changed = true;
                    }
                    if had_reaction {
                        history.unread_reactions.remove(&msg_id);
                        history.unread_reactions_count =
                            (history.unread_reactions_count - 1).max(0);
                        self.changes
                            .history_updated(p, HistoryUpdateFlags::UNREAD_REACTIONS);
                        unread_changed = true;
                    }
                }
                self.changes
                    .message_updated(full_id, MessageUpdateFlags::EDITED);
                if unread_changed {
                    self.sync_dialog_lists(p);
                }
            }
        }
        unknown
    }

    /// Marks unread reactions on specific visible messages in `peer_id` as read locally,
    /// matching `HistoryItem::markReactionsRead` (`history/history_item.cpp:1969-1983`).
    ///
    /// Only regular server messages that currently have unread reactions (flag, unread recent
    /// reactor, or membership in `history.unread_reactions`) are modified and returned.
    /// Does NOT clear unrelated `MEDIA_IS_UNREAD` or `MENTIONS_ME` flags.
    pub fn mark_messages_reactions_read(&mut self, peer_id: PeerId, ids: &[MsgId]) -> Vec<MsgId> {
        let mut marked = Vec::new();
        let mut seen = BTreeSet::new();
        for &msg_id in ids {
            if !msg_id.is_server() || !seen.insert(msg_id) {
                continue;
            }
            let full_id = FullMsgId::new(peer_id, msg_id);
            let in_history_set = self
                .histories
                .get(&peer_id)
                .is_some_and(|h| h.unread_reactions.contains(&msg_id));
            let (has_unread, topic_root, saved_sublist_peer) = match self.messages.get_mut(&full_id)
            {
                Some(msg) => {
                    let had_flag = msg.flags.contains(MessageFlags::HAS_UNREAD_REACTION);
                    let had_recent = msg
                        .reactions
                        .as_ref()
                        .is_some_and(|r| r.recent_reactors.iter().any(|rr| rr.unread));
                    if had_flag || had_recent || in_history_set {
                        msg.flags.remove(MessageFlags::HAS_UNREAD_REACTION);
                        if let Some(reactions) = msg.reactions.as_mut() {
                            for rr in &mut reactions.recent_reactors {
                                rr.unread = false;
                            }
                        }
                        (
                            true,
                            msg.topic_root_id(),
                            msg.forward_from.as_ref().and_then(|f| f.saved_from_peer),
                        )
                    } else {
                        (false, None, None)
                    }
                }
                None => (in_history_set, None, None),
            };
            if !has_unread {
                continue;
            }
            if let Some(history) = self.histories.get_mut(&peer_id) {
                history.unread_reactions.remove(&msg_id);
                history.unread_reactions_count = (history.unread_reactions_count - 1).max(0);
                self.changes
                    .history_updated(peer_id, HistoryUpdateFlags::UNREAD_REACTIONS);
            }
            if let Some(top_id) = topic_root
                && let Some(topic) = self
                    .forums
                    .get_mut(&peer_id)
                    .and_then(|f| f.topic_mut(top_id))
            {
                topic.unread_reactions_count = (topic.unread_reactions_count - 1).max(0);
            }
            if let Some(saved_peer) = saved_sublist_peer {
                if peer_id == self.self_peer_id() {
                    if let Some(sub) = self.saved_messages.sublist_mut(saved_peer) {
                        sub.unread_reactions_count = (sub.unread_reactions_count - 1).max(0);
                    }
                } else if let Some(ch_id) = peer_id.to_channel()
                    && let Some(sub) = self
                        .monoforums
                        .get_mut(&ch_id)
                        .and_then(|m| m.sublist_mut(saved_peer))
                {
                    sub.unread_reactions_count = (sub.unread_reactions_count - 1).max(0);
                }
            }
            self.changes.message_updated(
                full_id,
                MessageUpdateFlags::EDITED | MessageUpdateFlags::DIALOG_ROW_REPAINT,
            );
            marked.push(msg_id);
        }
        if !marked.is_empty() {
            self.sync_dialog_lists(peer_id);
        }
        marked
    }

    /// Clears cached history messages strictly older than `available_min_id` and records
    /// the new `available_min_id` (`updateChannelAvailableMessages`).
    pub fn clear_history_up_till(&mut self, peer_id: PeerId, available_min_id: MsgId) {
        if let Some(channel_id) = peer_id.to_channel()
            && let Some(ch) = self.channel_mut(channel_id)
        {
            ch.available_min_id = available_min_id;
        }
        let to_remove: Vec<MsgId> = self
            .peer_message_ids
            .get(&peer_id)
            .map(|ids| {
                ids.range(..available_min_id)
                    .copied()
                    .filter(|id| id.is_server())
                    .collect()
            })
            .unwrap_or_default();
        if let Some(history) = self.histories.get_mut(&peer_id) {
            history.available_min_id = available_min_id;
        }
        if !to_remove.is_empty() {
            self.delete_messages(peer_id, &to_remove);
        }
    }

    /// Returns a message by `FullMsgId`.
    #[inline]
    pub fn message(&self, id: FullMsgId) -> Option<&Message> {
        self.messages.get(&id)
    }

    /// Returns a mutable reference to a message by `FullMsgId`.
    #[inline]
    pub fn message_mut(&mut self, id: FullMsgId) -> Option<&mut Message> {
        let message = self.messages.get_mut(&id)?;
        self.media_dirty.insert(id);
        Some(message)
    }

    /// Looks up a non-channel message by its globally unique `MsgId`
    /// (`Data::Session::nonChannelMessage`).
    pub fn non_channel_message(&self, msg_id: MsgId) -> Option<&Message> {
        let peer_id = self.non_channel_messages.get(&msg_id)?;
        self.messages.get(&FullMsgId::new(*peer_id, msg_id))
    }

    /// Replaces an existing message with an edited version and broadcasts `EDITED`,
    /// keeping unread mention/reaction tracking consistent
    /// (`history/history_item.cpp:2440-2645`, `HistoryItem::applyEdition`).
    pub fn edit_message(&mut self, updated: Message) -> bool {
        let full_id = updated.full_id();
        let Some(prev) = self.messages.get(&full_id) else {
            return false;
        };
        let had_mention = prev.mentions_me();
        let had_reaction = prev.has_unread_reaction();
        let now_mention = updated.mentions_me();
        let now_reaction = updated.has_unread_reaction();
        let peer_id = updated.peer_id;
        let msg_id = updated.id;

        self.store_message(updated);

        let mut badges_changed = false;
        if let Some(history) = self.histories.get_mut(&peer_id) {
            if !had_mention && now_mention && history.unread_mentions.insert(msg_id) {
                history.unread_mentions_count += 1;
                self.changes
                    .history_updated(peer_id, HistoryUpdateFlags::UNREAD_MENTIONS);
                badges_changed = true;
            } else if had_mention && !now_mention {
                history.unread_mentions.remove(&msg_id);
                history.unread_mentions_count = (history.unread_mentions_count - 1).max(0);
                self.changes
                    .history_updated(peer_id, HistoryUpdateFlags::UNREAD_MENTIONS);
                badges_changed = true;
            }
            if !had_reaction && now_reaction && history.unread_reactions.insert(msg_id) {
                history.unread_reactions_count += 1;
                self.changes
                    .history_updated(peer_id, HistoryUpdateFlags::UNREAD_REACTIONS);
                badges_changed = true;
            } else if had_reaction && !now_reaction {
                history.unread_reactions.remove(&msg_id);
                history.unread_reactions_count = (history.unread_reactions_count - 1).max(0);
                self.changes
                    .history_updated(peer_id, HistoryUpdateFlags::UNREAD_REACTIONS);
                badges_changed = true;
            }
        }
        self.changes.message_updated(
            full_id,
            MessageUpdateFlags::EDITED | MessageUpdateFlags::DIALOG_ROW_REPAINT,
        );
        if badges_changed {
            self.sync_dialog_lists(peer_id);
        }
        true
    }

    /// Updates a message's pinned flag and broadcasts notifications.
    pub fn set_message_pinned(&mut self, id: FullMsgId, pinned: bool) -> bool {
        let Some(msg) = self.messages.get_mut(&id) else {
            return false;
        };
        if msg.is_pinned() == pinned {
            return false;
        }
        msg.flags.set(MessageFlags::PINNED, pinned);
        self.changes.message_updated(id, MessageUpdateFlags::EDITED);
        self.changes.entry_updated(
            DialogKey::History(id.peer),
            EntryUpdateFlags::HAS_PINNED_MESSAGES,
        );
        true
    }

    /// Updates a channel post's view count.
    pub fn update_message_views(&mut self, id: FullMsgId, views: i32) -> bool {
        let Some(msg) = self.messages.get_mut(&id) else {
            return false;
        };
        if msg.views == Some(views) {
            return false;
        }
        msg.views = Some(views);
        msg.flags.insert(MessageFlags::HAS_VIEWS);
        self.changes.message_updated(id, MessageUpdateFlags::EDITED);
        true
    }

    /// Updates a message's forward count.
    pub fn update_message_forwards(&mut self, id: FullMsgId, forwards: i32) -> bool {
        let Some(msg) = self.messages.get_mut(&id) else {
            return false;
        };
        if msg.forwards == Some(forwards) {
            return false;
        }
        msg.forwards = Some(forwards);
        self.changes.message_updated(id, MessageUpdateFlags::EDITED);
        true
    }

    /// Updates a message's reactions summary and keeps `HAS_UNREAD_REACTION` and
    /// `history.unread_reactions_count` in sync
    /// (`history/history_item.cpp:5566-5583`, `HistoryItem::updateReactions`).
    pub fn update_message_reactions(
        &mut self,
        id: FullMsgId,
        reactions: MessageReactionsSummary,
    ) -> bool {
        let Some(msg) = self.messages.get_mut(&id) else {
            return false;
        };
        let had_unread = msg.has_unread_reaction();
        let has_unread = reactions.recent_reactors.iter().any(|r| r.unread);
        msg.flags
            .set(MessageFlags::CAN_VIEW_REACTIONS, reactions.can_see_list);
        msg.flags.set(
            MessageFlags::REACTIONS_ARE_TAGS,
            reactions.reactions_as_tags,
        );
        msg.flags.set(MessageFlags::HAS_UNREAD_REACTION, has_unread);
        msg.reactions = Some(reactions);

        let mut badges_changed = false;
        if has_unread != had_unread
            && let Some(history) = self.histories.get_mut(&id.peer)
        {
            if has_unread {
                if history.unread_reactions.insert(id.msg) {
                    history.unread_reactions_count += 1;
                }
            } else {
                history.unread_reactions.remove(&id.msg);
                history.unread_reactions_count = (history.unread_reactions_count - 1).max(0);
            }
            self.changes
                .history_updated(id.peer, HistoryUpdateFlags::UNREAD_REACTIONS);
            badges_changed = true;
        }
        self.changes.message_updated(id, MessageUpdateFlags::EDITED);
        if badges_changed {
            self.sync_dialog_lists(id.peer);
        }
        true
    }

    /// Optimistically strips reactions by `participant_id` from one message (`Some(msg_id)`)
    /// or across all cached messages in `peer_id` (`None`), returning the number of modified
    /// messages.
    ///
    /// Port of `Data::Session::removeReactionsFromParticipant` (`data/data_session.cpp:3518-3544`)
    /// and `HistoryItem::removeReactionsFromParticipant` (`history/history_item.cpp:3903-3922`).
    pub fn remove_reactions_from_participant(
        &mut self,
        peer_id: PeerId,
        msg_id: Option<MsgId>,
        participant_id: PeerId,
        known_reaction: Option<&crate::peer::ReactionId>,
    ) -> usize {
        let targets: Vec<FullMsgId> = match msg_id {
            Some(mid) => {
                let full_id = FullMsgId::new(peer_id, mid);
                if self.messages.contains_key(&full_id) {
                    vec![full_id]
                } else {
                    Vec::new()
                }
            }
            None => self
                .messages
                .keys()
                .copied()
                .filter(|id| id.peer == peer_id)
                .collect(),
        };
        if targets.is_empty() {
            return 0;
        }

        let is_self = participant_id == self.self_peer_id();
        let mut modified = 0usize;
        let mut badges_changed = false;

        for full_id in targets {
            let Some(msg) = self.messages.get_mut(&full_id) else {
                continue;
            };
            let had_unread = msg.has_unread_reaction();
            let Some(reactions) = msg.reactions.as_mut() else {
                continue;
            };
            if !reactions.remove_from_participant(participant_id, known_reaction, is_self) {
                continue;
            }
            if reactions.is_empty() {
                msg.reactions = None;
                msg.flags.remove(MessageFlags::CAN_VIEW_REACTIONS);
            }
            let now_unread = msg
                .reactions
                .as_ref()
                .is_some_and(|r| r.recent_reactors.iter().any(|rr| rr.unread));
            msg.flags.set(MessageFlags::HAS_UNREAD_REACTION, now_unread);

            if had_unread
                && !now_unread
                && let Some(history) = self.histories.get_mut(&peer_id)
            {
                history.unread_reactions.remove(&full_id.msg);
                history.unread_reactions_count = (history.unread_reactions_count - 1).max(0);
                self.changes
                    .history_updated(peer_id, HistoryUpdateFlags::UNREAD_REACTIONS);
                badges_changed = true;
            }

            self.changes
                .message_updated(full_id, MessageUpdateFlags::EDITED);
            modified += 1;
        }

        if badges_changed {
            self.sync_dialog_lists(peer_id);
        }
        modified
    }

    /// Deletes messages in `peer_id` by `MsgId`, updating loaded ranges, unread counters,
    /// top message metadata, and notifying
    /// (`history/history.cpp:268-289`, `History::itemVanished`;
    /// `history/history_item.cpp:3229-3255`, `HistoryItem::destroyHistoryEntry`).
    pub fn delete_messages(&mut self, peer_id: PeerId, ids: &[MsgId]) -> usize {
        let mut deleted = 0;
        let mut top_changed = false;
        for &msg_id in ids {
            let full_id = FullMsgId::new(peer_id, msg_id);
            let removed = self.take_message(full_id);
            if removed.is_some() {
                deleted += 1;
                if !peer_id.is_channel() {
                    self.non_channel_messages.remove(&msg_id);
                }
                self.changes
                    .message_updated(full_id, MessageUpdateFlags::DESTROYED);
            }
            if let Some(history) = self.histories.get_mut(&peer_id) {
                history.loaded_ranges.remove_one(msg_id, false);
                let removed_mention = history.unread_mentions.remove(&msg_id);
                let removed_reaction = history.unread_reactions.remove(&msg_id);
                if let Some(ref msg) = removed {
                    if msg.mentions_me() || removed_mention {
                        history.unread_mentions_count = (history.unread_mentions_count - 1).max(0);
                        self.changes
                            .history_updated(peer_id, HistoryUpdateFlags::UNREAD_MENTIONS);
                    }
                    if msg.has_unread_reaction() || removed_reaction {
                        history.unread_reactions_count =
                            (history.unread_reactions_count - 1).max(0);
                        self.changes
                            .history_updated(peer_id, HistoryUpdateFlags::UNREAD_REACTIONS);
                    }
                    if (!msg.is_outgoing() || msg.is_post())
                        && history.is_server_side_unread(msg)
                        && let Some(count) = history.unread_count.as_mut()
                        && *count > 0
                    {
                        *count -= 1;
                        self.changes
                            .history_updated(peer_id, HistoryUpdateFlags::UNREAD_VIEW);
                    }
                } else {
                    if removed_mention {
                        history.unread_mentions_count = (history.unread_mentions_count - 1).max(0);
                        self.changes
                            .history_updated(peer_id, HistoryUpdateFlags::UNREAD_MENTIONS);
                    }
                    if removed_reaction {
                        history.unread_reactions_count =
                            (history.unread_reactions_count - 1).max(0);
                        self.changes
                            .history_updated(peer_id, HistoryUpdateFlags::UNREAD_REACTIONS);
                    }
                }
                if history.top_message_id == Some(msg_id) {
                    history.top_message_id = history.loaded_ranges.max_id();
                    top_changed = true;
                }
            }
        }
        if top_changed {
            // Fall back to `loaded_ranges.max_id()` or the highest remaining message in
            // `self.messages` for `peer_id` (e.g. pending/failed client messages not tracked in
            // `loaded_ranges`, matching `History::itemRemoved` / `lastAvailableMessage` in
            // `history/history.cpp:229-236`).
            let range_max = self
                .histories
                .get(&peer_id)
                .and_then(|h| h.loaded_ranges.max_id());
            let remaining_max = self
                .peer_message_ids
                .get(&peer_id)
                .and_then(|ids| ids.last().copied());
            let new_top = match (range_max, remaining_max) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            };
            let new_date = new_top
                .and_then(|id| self.messages.get(&FullMsgId::new(peer_id, id)))
                .map_or(0, |m| m.date);
            if let Some(history) = self.histories.get_mut(&peer_id) {
                history.top_message_id = new_top;
                history.chat_list_time_id = new_date;
            }
        }
        if deleted > 0 {
            self.sync_dialog_lists(peer_id);
        }
        deleted
    }

    /// Deletes non-channel messages by their globally unique `MsgId`s
    /// (`Data::Session::processNonChannelMessagesDeleted`).
    pub fn delete_non_channel_messages(&mut self, ids: &[MsgId]) -> usize {
        let mut by_peer: FxHashMap<PeerId, Vec<MsgId>> = FxHashMap::default();
        for &msg_id in ids {
            if let Some(&peer_id) = self.non_channel_messages.get(&msg_id) {
                by_peer.entry(peer_id).or_default().push(msg_id);
            }
        }
        let mut total = 0;
        for (peer_id, peer_ids) in by_peer {
            total += self.delete_messages(peer_id, &peer_ids);
        }
        total
    }

    // -------------------------------------------------------------------------
    // Histories, Dialogs, Folders & Filters
    // -------------------------------------------------------------------------

    /// Ensures a `History` exists for `peer_id` and returns a mutable reference.
    pub fn ensure_history(&mut self, peer_id: PeerId) -> &mut History {
        self.histories
            .entry(peer_id)
            .or_insert_with(|| History::new(peer_id))
    }

    /// Returns a loaded `History` for `peer_id`.
    #[inline]
    pub fn history(&self, peer_id: PeerId) -> Option<&History> {
        self.histories.get(&peer_id)
    }

    /// Returns mutable access to a loaded `History` for `peer_id`.
    #[inline]
    pub fn history_mut(&mut self, peer_id: PeerId) -> Option<&mut History> {
        self.histories.get_mut(&peer_id)
    }

    /// Assigns `peer_id`'s history to `folder_id` (`None` = main list, `Some(1)` = archive),
    /// unpinning it from its previous folder first (`History::setFolderPointer` in
    /// `history/history.cpp:2528-2534`), moving it between dialog lists, and updating filters
    /// & badges.
    pub fn set_history_folder(&mut self, peer_id: PeerId, folder_id: Option<FolderId>) {
        let target = folder_id.filter(|&id| id != 0);
        let already_same = self
            .histories
            .get(&peer_id)
            .is_some_and(|h| h.folder == Some(target));
        if already_same {
            return;
        }
        // Unpin from old folder's pinned list before switching `history.folder`
        // (`history/history.cpp:2532-2534`, `History::setFolderPointer`).
        if self.histories.get(&peer_id).is_some_and(|h| h.is_pinned(0)) {
            self.set_pinned(DialogKey::History(peer_id), 0, false);
        }
        let history = self.ensure_history(peer_id);
        history.folder = Some(target);
        history.in_chat_list = true;
        self.changes
            .history_updated(peer_id, HistoryUpdateFlags::FOLDER);
        self.sync_dialog_lists(peer_id);
    }

    /// Updates mute status on `peer_id`'s history and recomputes unread counters & filters.
    pub fn set_history_muted(&mut self, peer_id: PeerId, muted: bool) {
        let history = self.ensure_history(peer_id);
        if history.muted == muted {
            return;
        }
        history.muted = muted;
        self.changes
            .peer_updated(peer_id, PeerUpdateFlags::NOTIFICATIONS);
        self.changes
            .history_updated(peer_id, HistoryUpdateFlags::UNREAD_VIEW);
        self.sync_dialog_lists(peer_id);
    }

    /// Sets the unread count on `peer_id`'s history and updates dialog lists & filters.
    pub fn set_history_unread_count(&mut self, peer_id: PeerId, unread_count: i32) {
        let history = self.ensure_history(peer_id);
        let count = unread_count.max(0);
        if history.unread_count == Some(count) {
            return;
        }
        history.unread_count = Some(count);
        if count > 0 {
            history.unread_mark = false;
        }
        self.changes
            .history_updated(peer_id, HistoryUpdateFlags::UNREAD_VIEW);
        self.sync_dialog_lists(peer_id);
    }

    /// Sets or clears the explicit "Mark as Unread" flag on `peer_id`'s history.
    pub fn set_history_unread_mark(&mut self, peer_id: PeerId, unread_mark: bool) {
        let history = self.ensure_history(peer_id);
        if history.unread_mark == unread_mark {
            return;
        }
        history.unread_mark = unread_mark;
        self.changes
            .history_updated(peer_id, HistoryUpdateFlags::UNREAD_VIEW);
        self.sync_dialog_lists(peer_id);
    }

    /// Advances inbox read watermark for `peer_id`, counting only incoming regular messages
    /// when `still_unread` is unknown (`History::inboxRead`, `history/history.cpp:2291-2309`).
    /// When the count cannot be computed from loaded messages, the dialog entry is queued for
    /// [`Self::take_dialog_entry_requests`] so the server's count replaces the old one.
    pub fn mark_inbox_read(&mut self, peer_id: PeerId, up_to: MsgId, still_unread: Option<i32>) {
        let messages = &self.messages;
        let history = self
            .histories
            .entry(peer_id)
            .or_insert_with(|| History::new(peer_id));
        let outcome = history.inbox_read_with_filter(up_to, still_unread, |id| {
            is_unread_candidate(messages, FullMsgId::new(peer_id, id))
        });
        if outcome == InboxReadOutcome::CountUnknown {
            self.unread_count_requests.insert(peer_id);
        }
        self.changes
            .history_updated(peer_id, HistoryUpdateFlags::UNREAD_VIEW);
        self.sync_dialog_lists(peer_id);
    }

    /// The local half of a read the user made (`Data::Histories::readInboxTill`,
    /// `data/data_histories.cpp:260-352`): when the messages still unread after `up_to` can be
    /// counted from loaded messages, the read mark and the count change now and `true` is
    /// returned. Otherwise nothing changes: the read goes to the server, and its answer applies
    /// the read through [`Self::mark_inbox_read`], which requests the dialog entry for the count.
    pub fn read_inbox_till_local(&mut self, peer_id: PeerId, up_to: MsgId) -> bool {
        let messages = &self.messages;
        let still_unread = self.histories.get(&peer_id).and_then(|history| {
            history.count_still_unread_local(up_to, |id| {
                is_unread_candidate(messages, FullMsgId::new(peer_id, id))
            })
        });
        let Some(still_unread) = still_unread else {
            return false;
        };
        self.mark_inbox_read(peer_id, up_to, Some(still_unread));
        true
    }

    /// Advances outbox read watermark for `peer_id`.
    pub fn mark_outbox_read(&mut self, peer_id: PeerId, up_to: MsgId) {
        let history = self.ensure_history(peer_id);
        history.outbox_read(up_to);
        self.changes
            .history_updated(peer_id, HistoryUpdateFlags::OUTBOX_READ);
    }

    /// Sets a draft on `peer_id`'s history and refreshes dialog sort order if applicable.
    pub fn set_draft(&mut self, peer_id: PeerId, key: DraftKey, draft: Draft) {
        let is_cloud = key.is_cloud();
        let history = self.ensure_history(peer_id);
        history.set_draft(key, draft);
        if is_cloud {
            self.changes
                .history_updated(peer_id, HistoryUpdateFlags::CLOUD_DRAFT);
        } else {
            self.changes.entry_updated(
                DialogKey::History(peer_id),
                EntryUpdateFlags::LOCAL_DRAFT_SET,
            );
        }
        self.sync_dialog_lists(peer_id);
    }

    /// Clears a draft on `peer_id`'s history.
    pub fn clear_draft(&mut self, peer_id: PeerId, key: DraftKey) -> Option<Draft> {
        let is_cloud = key.is_cloud();
        let removed = self.histories.get_mut(&peer_id)?.clear_draft(key);
        if removed.is_some() {
            if is_cloud {
                self.changes
                    .history_updated(peer_id, HistoryUpdateFlags::CLOUD_DRAFT);
            } else {
                self.changes.entry_updated(
                    DialogKey::History(peer_id),
                    EntryUpdateFlags::LOCAL_DRAFT_SET,
                );
            }
            self.sync_dialog_lists(peer_id);
        }
        removed
    }

    /// Pins or unpins `key` in the main list (`filter_id == 0`, `archived == false`),
    /// archive list (`filter_id == 0`, `archived == true`), or a custom filter (`filter_id != 0`).
    pub fn set_pinned(&mut self, key: DialogKey, filter_id: FilterId, pinned: bool) {
        let archived = match key {
            DialogKey::History(peer_id) => self
                .histories
                .get(&peer_id)
                .is_some_and(History::is_archived),
            _ => false,
        };

        let affected: Vec<DialogKey> = if filter_id != 0 {
            let Some(list) = self.filters.chats_list_mut(filter_id) else {
                return;
            };
            let mut changed = list.pinned_mut().set_pinned(key, pinned);
            changed.extend(list.pinned().order().iter().copied());
            changed.push(key);
            changed
        } else if archived {
            let mut changed = self.archive.chats_list.pinned_mut().set_pinned(key, pinned);
            changed.extend(self.archive.chats_list.pinned().order().iter().copied());
            changed.push(key);
            changed
        } else {
            let mut changed = self.main_list.pinned_mut().set_pinned(key, pinned);
            changed.extend(self.main_list.pinned().order().iter().copied());
            changed.push(key);
            changed
        };

        self.sync_pinned_indices(filter_id, archived, &affected);
    }

    /// Appends `key` to the pinned list in server order when `pinned == true` (instead of
    /// rotating to top), or removes it when `pinned == false` (`Session::setPinnedFromEntryList`
    /// in `data/data_session.cpp:2801-2810`).
    pub fn add_pinned_from_entry_list(&mut self, key: DialogKey, archived: bool, pinned: bool) {
        let mut affected = if archived {
            if pinned {
                self.archive.chats_list.pinned_mut().add_pinned(key)
            } else {
                self.archive.chats_list.pinned_mut().set_pinned(key, false)
            }
        } else if pinned {
            self.main_list.pinned_mut().add_pinned(key)
        } else {
            self.main_list.pinned_mut().set_pinned(key, false)
        };
        if archived {
            affected.extend(self.archive.chats_list.pinned().order().iter().copied());
        } else {
            affected.extend(self.main_list.pinned().order().iter().copied());
        }
        affected.push(key);
        self.sync_pinned_indices(0, archived, &affected);
    }

    /// Clears all pinned entries in the main list (`archived == false`) or archive folder
    /// (`archived == true`) and resets cached pinned indices (`Session::clearPinnedChats` in
    /// `data/data_session.cpp:3048-3053`).
    pub fn clear_pinned_chats(&mut self, archived: bool) {
        let removed = if archived {
            self.archive.chats_list.pinned_mut().clear()
        } else {
            self.main_list.pinned_mut().clear()
        };
        if !removed.is_empty() {
            self.sync_pinned_indices(0, archived, &removed);
        }
    }

    /// Pins or unpins `key` in the explicit folder (`archived == false` for main,
    /// `archived == true` for archive), updating `history.folder` if not yet known
    /// (`History::applyPinnedUpdate` in `history/history.cpp:2654-2664`).
    pub fn set_pinned_in_folder(&mut self, key: DialogKey, archived: bool, pinned: bool) {
        if let DialogKey::History(peer_id) = key
            && let Some(history) = self.histories.get_mut(&peer_id)
            && !history.folder_known()
        {
            history.set_folder(archived.then_some(ARCHIVE_FOLDER_ID));
        }
        let mut affected = if archived {
            let mut changed = self.archive.chats_list.pinned_mut().set_pinned(key, pinned);
            changed.extend(self.archive.chats_list.pinned().order().iter().copied());
            changed
        } else {
            let mut changed = self.main_list.pinned_mut().set_pinned(key, pinned);
            changed.extend(self.main_list.pinned().order().iter().copied());
            changed
        };
        affected.push(key);
        self.sync_pinned_indices(0, archived, &affected);
    }

    /// Replaces the entire pinned order for `filter_id` (or archive when `archived == true`).
    pub fn apply_pinned_list(
        &mut self,
        filter_id: FilterId,
        archived: bool,
        order: Vec<DialogKey>,
    ) {
        let mut affected = if filter_id != 0 {
            let Some(list) = self.filters.chats_list_mut(filter_id) else {
                return;
            };
            list.pinned_mut().apply_list(order.clone())
        } else if archived {
            self.archive
                .chats_list
                .pinned_mut()
                .apply_list(order.clone())
        } else {
            self.main_list.pinned_mut().apply_list(order.clone())
        };
        affected.extend(order);
        self.sync_pinned_indices(filter_id, archived, &affected);
    }

    /// Inserts or updates a chat filter (folder) and populates its dialog list
    /// from all currently known histories.
    pub fn set_filter(&mut self, filter: ChatFilter) {
        let filter_id = filter.id;
        let mut pinned_keys: Vec<DialogKey> = filter
            .pinned
            .iter()
            .copied()
            .map(DialogKey::History)
            .collect();
        // Also clear cached pinned_index(filter_id) for any previously pinned peers that
        // were removed by the updated filter (`data/data_chat_filters.cpp:780-782`).
        let evicted = self.filters.upsert(filter);
        pinned_keys.extend(evicted);
        self.sync_pinned_indices(filter_id, false, &pinned_keys);
        let peer_ids: Vec<PeerId> = self.histories.keys().copied().collect();
        for peer_id in peer_ids {
            self.refresh_history_in_filter(peer_id, filter_id);
        }
        self.changes.filters_changed();
    }

    /// Replaces the entire set of custom chat filters from a full `messages.getDialogFilters`
    /// snapshot (`ChatFilters::received` in `data/data_chat_filters.cpp:491-525`): removes
    /// any filters absent from `filters`, upserts the rest, and applies their exact order.
    pub fn sync_filters_from_list(&mut self, filters: Vec<ChatFilter>, tags_enabled: bool) {
        self.filters.set_tags_enabled(tags_enabled);
        let incoming_ids: BTreeSet<FilterId> = filters.iter().map(|f| f.id).collect();
        let to_remove: Vec<FilterId> = self
            .filters
            .list()
            .iter()
            .map(|f| f.id)
            .filter(|id| !incoming_ids.contains(id))
            .collect();
        for id in to_remove {
            self.remove_filter(id);
        }
        let order: Vec<FilterId> = filters.iter().map(|f| f.id).collect();
        for filter in filters {
            self.set_filter(filter);
        }
        self.apply_filter_order(&order);
    }

    /// Removes a chat filter by ID.
    pub fn remove_filter(&mut self, filter_id: FilterId) -> bool {
        if !self.filters.remove(filter_id) {
            return false;
        }
        for history in self.histories.values_mut() {
            history.set_pinned_index(filter_id, 0);
        }
        self.changes.filters_changed();
        true
    }

    /// Applies a new ordering of custom chat filters (`updateDialogFilterOrder`).
    pub fn apply_filter_order(&mut self, order: &[FilterId]) {
        self.filters.apply_order(order);
        self.changes.filters_changed();
    }

    /// Applies a new ordering of custom chat filters strictly matching `ChatFilters::applyOrder`
    /// (`data/data_chat_filters.cpp:799-835`). Returns `false` if `order` does not match the
    /// loaded filters (caller should reload via `messages.getDialogFilters`).
    pub fn apply_filter_order_checked(&mut self, order: &[FilterId]) -> bool {
        if self.filters.apply_order_checked(order) {
            self.changes.filters_changed();
            true
        } else {
            false
        }
    }

    /// Marks the main dialog list (`folder_id == 0`) or archive folder (`folder_id == 1`) as
    /// fully loaded (`Session::chatsListDone` in `apiwrap.cpp:1128`).
    pub fn set_folder_loaded(&mut self, folder_id: FolderId, loaded: bool) {
        if folder_id == ARCHIVE_FOLDER_ID {
            self.archive.chats_list.set_loaded(loaded);
        } else if folder_id == 0 {
            self.main_list.set_loaded(loaded);
        }
    }

    /// Records server-reported total dialog count for `folder_id`
    /// (`Session::applyDialogs` in `data/data_session.cpp:2856-2858`).
    pub fn set_folder_cloud_list_size(&mut self, folder_id: FolderId, size: usize) {
        if folder_id == ARCHIVE_FOLDER_ID {
            self.archive.chats_list.set_cloud_list_size(size);
        } else if folder_id == 0 {
            self.main_list.set_cloud_list_size(size);
        }
    }

    /// Re-synchronizes the archive folder entry inside the main dialog list.
    pub fn refresh_archive_entry(&mut self) {
        self.sync_archive_entry();
    }

    /// Sets whether colored folder tags are enabled on custom filters.
    pub fn set_filters_tags_enabled(&mut self, enabled: bool) {
        self.filters.set_tags_enabled(enabled);
        self.changes.filters_changed();
    }

    /// Returns the main chats `MainList`.
    #[inline]
    pub fn main_list(&self) -> &MainList {
        &self.main_list
    }

    /// Returns the archive `Folder`.
    #[inline]
    pub fn archive(&self) -> &Folder {
        &self.archive
    }

    /// Returns mutable access to the archive `Folder` (e.g. for `update_cloud_unread`).
    #[inline]
    pub fn archive_mut(&mut self) -> &mut Folder {
        &mut self.archive
    }

    /// Returns the `ChatFilters` manager.
    #[inline]
    pub fn filters(&self) -> &ChatFilters {
        &self.filters
    }

    /// Returns ordered `DialogKey`s in the main chats list.
    #[inline]
    pub fn main_dialogs(&self) -> &[DialogKey] {
        self.main_list.indexed().keys()
    }

    /// Returns ordered `DialogKey`s in the archive folder.
    #[inline]
    pub fn archive_dialogs(&self) -> &[DialogKey] {
        self.archive.chats_list.indexed().keys()
    }

    /// Returns ordered `DialogKey`s in custom filter `filter_id`.
    #[inline]
    pub fn filter_dialogs(&self, filter_id: FilterId) -> Option<&[DialogKey]> {
        self.filters
            .chats_list(filter_id)
            .map(|list| list.indexed().keys())
    }

    /// Computes the global application unread badge (main list, respecting `badge_settings`).
    pub fn total_unread_badge(&self) -> BadgesState {
        badges_for_unread(
            &self.main_list.unread_state(),
            CountInBadge::Default,
            IncludeInBadge::Default,
            self.badge_settings,
        )
    }

    /// Computes the unread badge for custom filter `filter_id`.
    pub fn filter_unread_badge(&self, filter_id: FilterId) -> Option<BadgesState> {
        self.filters.unread_badge(filter_id, self.badge_settings)
    }

    /// Re-synchronizes a single history across the main list, archive folder,
    /// and all custom filters.
    pub fn sync_dialog_lists(&mut self, peer_id: PeerId) {
        let Some(history) = self.histories.get(&peer_id) else {
            return;
        };
        // `History::shouldBeInChatList` (`history/history.cpp:3464-3466`): a history whose
        // folder is not known yet could belong to the archive, so it stays out of every list
        // until its dialog entry arrives.
        if !history.in_chat_list || !history.folder_known() {
            let key = DialogKey::History(peer_id);
            self.main_list.remove_entry(key);
            self.archive.chats_list.remove_entry(key);
            let filter_ids: Vec<FilterId> = self.filters.list().iter().map(|f| f.id).collect();
            for fid in filter_ids {
                if let Some(list) = self.filters.chats_list_mut(fid) {
                    list.remove_entry(key);
                }
            }
            self.sync_archive_entry();
            return;
        }

        let key = DialogKey::History(peer_id);
        let is_archived = history.is_archived();
        let unread = history.unread_state();
        let date = history.adjusted_chat_list_time_id();
        let fixed_index = history.fixed_on_top_index();
        let pinned_main = history.pinned_index(0);
        let unread_on_top = self.dialogs_unread_on_top && unread.has_unread_unmuted_for_sort();

        self.sort_shift = self.sort_shift.wrapping_add(1).max(1);
        let shift = self.sort_shift;
        let sort_key_main =
            compute_dialog_sort_key(date, shift, fixed_index, pinned_main, unread_on_top);

        let (name_sort_key, name_words, first_letters) = self.peer_search_meta(peer_id);

        if is_archived {
            self.main_list.remove_entry(key);
            self.archive.chats_list.upsert_entry(
                key,
                sort_key_main,
                name_sort_key,
                name_words,
                first_letters,
                unread,
            );
        } else {
            self.archive.chats_list.remove_entry(key);
            self.main_list.upsert_entry(
                key,
                sort_key_main,
                name_sort_key,
                name_words,
                first_letters,
                unread,
            );
        }

        self.sync_archive_entry();

        let filter_ids: Vec<FilterId> = self.filters.list().iter().map(|f| f.id).collect();
        for fid in filter_ids {
            self.refresh_history_in_filter(peer_id, fid);
        }
    }

    fn sync_archive_entry(&mut self) {
        let archive_key = DialogKey::Folder(self.archive.id);
        if self.archive.should_be_in_chat_list() {
            let sort_key = compute_dialog_sort_key(
                0,
                0,
                self.archive.fixed_on_top_index(),
                self.main_list.pinned().pinned_index(archive_key),
                false,
            );
            // Archive entry does not contribute to main list unread counts directly
            // in the same way (or counts as muted); match tdesktop Folder::chatListUnreadState.
            let archive_unread = UnreadState {
                known: true,
                ..Default::default()
            };
            self.main_list.upsert_entry(
                archive_key,
                sort_key,
                self.archive.name.to_lowercase(),
                BTreeSet::new(),
                BTreeSet::new(),
                archive_unread,
            );
        } else {
            self.main_list.remove_entry(archive_key);
        }
    }

    fn refresh_history_in_filter(&mut self, peer_id: PeerId, filter_id: FilterId) {
        let key = DialogKey::History(peer_id);
        let (matches, unread, sort_key) = {
            let (Some(peer), Some(history), Some(filter)) = (
                self.peers.get(&peer_id),
                self.histories.get(&peer_id),
                self.filters.get(filter_id),
            ) else {
                if let Some(list) = self.filters.chats_list_mut(filter_id) {
                    list.remove_entry(key);
                }
                return;
            };
            let matches = history.in_chat_list && filter.contains(peer, history, false);
            let unread = history.unread_state();
            let date = history.adjusted_chat_list_time_id();
            let pinned = history.pinned_index(filter_id);
            let unread_on_top = self.dialogs_unread_on_top && unread.has_unread_unmuted_for_sort();
            self.sort_shift = self.sort_shift.wrapping_add(1).max(1);
            let sort_key = compute_dialog_sort_key(date, self.sort_shift, 0, pinned, unread_on_top);
            (matches, unread, sort_key)
        };

        let (name_sort_key, name_words, first_letters) = self.peer_search_meta(peer_id);
        if let Some(list) = self.filters.chats_list_mut(filter_id) {
            if matches {
                list.upsert_entry(
                    key,
                    sort_key,
                    name_sort_key,
                    name_words,
                    first_letters,
                    unread,
                );
            } else {
                list.remove_entry(key);
            }
        }
    }

    fn sync_pinned_indices(&mut self, filter_id: FilterId, archived: bool, affected: &[DialogKey]) {
        let unique: BTreeSet<DialogKey> = affected.iter().copied().collect();
        for key in unique {
            let new_index = if filter_id != 0 {
                self.filters
                    .chats_list(filter_id)
                    .map_or(0, |l| l.pinned().pinned_index(key))
            } else if archived {
                self.archive.chats_list.pinned().pinned_index(key)
            } else {
                self.main_list.pinned().pinned_index(key)
            };

            if let DialogKey::History(peer_id) = key {
                if let Some(history) = self.histories.get_mut(&peer_id)
                    && history.set_pinned_index(filter_id, new_index)
                {
                    self.changes
                        .history_updated(peer_id, HistoryUpdateFlags::IS_PINNED);
                }
                self.sync_dialog_lists(peer_id);
            }
        }
    }

    // -------------------------------------------------------------------------
    // Forum Topics, Saved Messages Sublists, Scheduled Messages & Quick Replies
    // -------------------------------------------------------------------------

    /// Returns the loaded [`ForumState`] for `peer_id`, if any.
    #[inline]
    #[must_use]
    pub fn forum(&self, peer_id: PeerId) -> Option<&ForumState> {
        self.forums.get(&peer_id)
    }

    /// Returns mutable access to the loaded [`ForumState`] for `peer_id`, if any.
    #[inline]
    pub fn forum_mut(&mut self, peer_id: PeerId) -> Option<&mut ForumState> {
        self.forums.get_mut(&peer_id)
    }

    /// Ensures a [`ForumState`] exists for `peer_id` and returns mutable access to it.
    pub fn ensure_forum(&mut self, peer_id: PeerId) -> &mut ForumState {
        self.forums
            .entry(peer_id)
            .or_insert_with(|| ForumState::new(peer_id))
    }

    /// Returns a loaded [`ForumTopicData`] for `(peer_id, root_id)`, if any.
    #[must_use]
    pub fn forum_topic(&self, peer_id: PeerId, root_id: MsgId) -> Option<&ForumTopicData> {
        self.forums.get(&peer_id)?.topic(root_id)
    }

    /// Applies an inline `updateEditForumTopic` onto a loaded topic and broadcasts
    /// [`ChangeEvent::TopicUpdated`] if any field changed.
    pub fn edit_forum_topic(
        &mut self,
        peer_id: PeerId,
        root_id: MsgId,
        title: Option<String>,
        icon_emoji_id: Option<i64>,
        closed: Option<bool>,
        hidden: Option<bool>,
    ) -> bool {
        let flags = self
            .forums
            .get_mut(&peer_id)
            .and_then(|f| f.apply_topic_edit(root_id, title, icon_emoji_id, closed, hidden));
        if let Some(flags) = flags {
            self.changes.topic_updated(peer_id, root_id, flags);
            true
        } else {
            false
        }
    }

    /// Removes a deleted forum topic from `peer_id` and emits [`TopicUpdateFlags::DESTROYED`].
    pub fn delete_forum_topic(
        &mut self,
        peer_id: PeerId,
        root_id: MsgId,
        preserve_general: bool,
    ) -> bool {
        let removed = self
            .forums
            .get_mut(&peer_id)
            .is_some_and(|f| f.apply_topic_deleted(root_id, preserve_general));
        if removed {
            self.changes
                .topic_updated(peer_id, root_id, TopicUpdateFlags::DESTROYED);
        }
        removed
    }

    /// Updates a forum topic's `read_inbox_max_id` (and optionally `unread_count`)
    /// (`updateReadChannelDiscussionInbox` / `messages.readDiscussion`).
    pub fn mark_topic_inbox_read(
        &mut self,
        peer_id: PeerId,
        root_id: MsgId,
        read_max_id: MsgId,
        unread_count: Option<i32>,
    ) -> bool {
        let Some(topic) = self
            .forums
            .get_mut(&peer_id)
            .and_then(|f| f.topic_mut(root_id))
        else {
            return false;
        };
        topic.apply_inbox_read_till(read_max_id, unread_count);
        self.changes
            .topic_updated(peer_id, root_id, TopicUpdateFlags::UNREAD_VIEW);
        true
    }

    /// Updates a forum topic's `read_outbox_max_id` (`updateReadChannelDiscussionOutbox`).
    pub fn mark_topic_outbox_read(
        &mut self,
        peer_id: PeerId,
        root_id: MsgId,
        read_max_id: MsgId,
    ) -> bool {
        let Some(topic) = self
            .forums
            .get_mut(&peer_id)
            .and_then(|f| f.topic_mut(root_id))
        else {
            return false;
        };
        topic.apply_outbox_read_till(read_max_id);
        self.changes
            .topic_updated(peer_id, root_id, TopicUpdateFlags::UNREAD_VIEW);
        true
    }

    /// Returns the account's own Saved Messages state (`Data::SavedMessages`).
    #[inline]
    #[must_use]
    pub fn saved_messages(&self) -> &SavedMessagesState {
        &self.saved_messages
    }

    /// Returns mutable access to the account's own Saved Messages state.
    #[inline]
    pub fn saved_messages_mut(&mut self) -> &mut SavedMessagesState {
        &mut self.saved_messages
    }

    /// Returns the monoforum [`SavedMessagesState`] for `channel_id`, if loaded.
    #[inline]
    #[must_use]
    pub fn monoforum(&self, channel_id: ChannelId) -> Option<&SavedMessagesState> {
        self.monoforums.get(&channel_id)
    }

    /// Ensures a monoforum [`SavedMessagesState`] exists for `channel_id`.
    pub fn ensure_monoforum(&mut self, channel_id: ChannelId) -> &mut SavedMessagesState {
        self.monoforums
            .entry(channel_id)
            .or_insert_with(|| SavedMessagesState::new(Some(channel_id)))
    }

    /// Returns Saved Messages reaction tags state (`Reactions::_myTags`).
    #[inline]
    #[must_use]
    pub fn saved_reaction_tags(&self) -> &SavedReactionTagsState {
        &self.saved_reaction_tags
    }

    /// Returns mutable access to Saved Messages reaction tags state.
    #[inline]
    pub fn saved_reaction_tags_mut(&mut self) -> &mut SavedReactionTagsState {
        &mut self.saved_reaction_tags
    }

    /// Returns the account-wide scheduled messages store (`Data::ScheduledMessages`).
    #[inline]
    #[must_use]
    pub fn scheduled_messages(&self) -> &ScheduledMessagesStore {
        &self.scheduled_messages
    }

    /// Returns mutable access to the account-wide scheduled messages store.
    #[inline]
    pub fn scheduled_messages_mut(&mut self) -> &mut ScheduledMessagesStore {
        &mut self.scheduled_messages
    }

    /// Returns the Business quick-reply shortcuts store (`Data::ShortcutMessages`).
    #[inline]
    #[must_use]
    pub fn shortcut_messages(&self) -> &ShortcutMessagesStore {
        &self.shortcut_messages
    }

    /// Returns mutable access to the Business quick-reply shortcuts store.
    #[inline]
    pub fn shortcut_messages_mut(&mut self) -> &mut ShortcutMessagesStore {
        &mut self.shortcut_messages
    }

    fn peer_search_meta(&self, peer_id: PeerId) -> (String, BTreeSet<String>, BTreeSet<char>) {
        if let Some(peer) = self.peers.get(&peer_id) {
            (
                peer.name().to_lowercase(),
                peer.common().name_words.clone(),
                peer.common().name_first_letters.clone(),
            )
        } else {
            (String::new(), BTreeSet::new(), BTreeSet::new())
        }
    }
}
