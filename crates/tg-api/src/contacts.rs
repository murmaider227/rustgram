//! Contacts loading, username resolution caching, blocked peers pagination, and
//! account privacy rules (`contacts.*`, `account.getPrivacy`/`setPrivacy`).
//!
//! Port origin:
//! - `Telegram/SourceFiles/apiwrap.cpp` (`requestContacts`)
//! - `Telegram/SourceFiles/window/window_session_controller.cpp` (`resolveUsername`)
//! - `Telegram/SourceFiles/api/api_blocked_peers.h` / `api_blocked_peers.cpp`
//! - `Telegram/SourceFiles/api/api_user_privacy.h` / `api_user_privacy.cpp`

use std::collections::HashMap;

use tg_data::{
    ChannelId, ChatId, ContactStatus, PeerId, PeerUpdateFlags, Session, TimeId, UserFlags, UserId,
};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::convert::{apply_tl_users_and_chats, input_peer_for, input_user_for, peer_id_from_tl};
use crate::error::ApiError;
use crate::hash::count_contacts_hash;
use crate::invoker::Invoker;

// ============================================================================
// Contacts (`apiwrap.cpp:927-953`)
// ============================================================================

/// Result of a `contacts.getContacts` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContactsFetchResult {
    /// Server returned `contacts.contactsNotModified`; cached contact list is up to date.
    NotModified,
    /// Server returned a full `contacts.contacts` list.
    Updated {
        /// Contact bare user IDs returned by the server (in server order).
        contact_user_ids: Vec<UserId>,
        /// Total saved contact count reported by the server (`saved_count`).
        saved_count: i32,
    },
}

/// Stateful contacts loader supporting both initial load (`hash = 0`, matching
/// `ApiWrap::requestContacts` in `apiwrap.cpp:927-953`) and hash-validated refreshes
/// (`contacts.contactsNotModified`).
#[derive(Debug, Default, Clone)]
pub struct ContactsManager {
    contact_ids: Vec<u64>,
    saved_count: i32,
    loaded: bool,
}

impl ContactsManager {
    /// Creates a new `ContactsManager`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns whether contacts have been loaded at least once.
    #[must_use]
    pub const fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// Returns the cached sorted bare user IDs of contacts.
    #[must_use]
    pub fn cached_contact_ids(&self) -> &[u64] {
        &self.contact_ids
    }

    /// Returns the last server-reported `saved_count`.
    #[must_use]
    pub const fn saved_count(&self) -> i32 {
        self.saved_count
    }

    /// Computes the current 64-bit `contacts.getContacts` hash for cached contacts
    /// (`0` if contacts have not been loaded yet, matching `apiwrap.cpp:931`).
    #[must_use]
    pub fn current_hash(&self) -> i64 {
        if !self.loaded {
            0
        } else {
            count_contacts_hash(&self.contact_ids)
        }
    }

    /// Fetches contacts via `contacts.getContacts`:
    /// - Sends `hash = 0` on initial load (`!self.loaded`), or `count_contacts_hash` on refresh.
    /// - Applies returned users into `session`, marks `self` as contact if present in `contacts`
    ///   (`apiwrap.cpp:941-948`), and preserves cached state on `contacts.contactsNotModified`.
    pub async fn fetch_contacts<I: Invoker>(
        &mut self,
        invoker: &I,
        session: &mut Session,
    ) -> Result<ContactsFetchResult, ApiError> {
        let hash = self.current_hash();
        let resp = invoker
            .contacts_get_contacts(tl_fns::contacts::GetContacts { hash })
            .await?;
        match resp {
            tl_enums::contacts::Contacts::NotModified => Ok(ContactsFetchResult::NotModified),
            tl_enums::contacts::Contacts::Contacts(data) => {
                apply_tl_users_and_chats(session, &data.users, &[]);
                let self_id = session.self_user_id();
                let mut contact_user_ids = Vec::with_capacity(data.contacts.len());
                let mut bare_ids = Vec::with_capacity(data.contacts.len());
                for c in &data.contacts {
                    let tl_enums::Contact::Contact(contact) = c;
                    let uid = UserId::from_tl(contact.user_id);
                    contact_user_ids.push(uid);
                    bare_ids.push(uid.0);
                    if uid == self_id
                        && let Some(self_user) = session.user_mut(self_id)
                    {
                        self_user.contact_status = ContactStatus::Contact;
                        self_user.flags.insert(UserFlags::CONTACT);
                        session
                            .changes()
                            .peer_updated(PeerId::from_user(self_id), PeerUpdateFlags::IS_CONTACT);
                    }
                }
                bare_ids.sort_unstable();
                bare_ids.dedup();
                self.contact_ids = bare_ids;
                self.saved_count = data.saved_count;
                self.loaded = true;
                session.set_contacts_loaded(true);
                Ok(ContactsFetchResult::Updated {
                    contact_user_ids,
                    saved_count: data.saved_count,
                })
            }
        }
    }
}

// ============================================================================
// Username resolution with caching (`window_session_controller.cpp:573-621`)
// ============================================================================

/// Result of resolving a username via cache or `contacts.resolveUsername`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedUsername {
    /// Resolved peer ID (now guaranteed to be in `session`).
    pub peer_id: PeerId,
    /// `true` if served directly from `Session::peer_by_username` without an RPC.
    pub from_cache: bool,
}

/// Resolves `username` (with optional `referer` token) following
/// `SessionNavigation::resolveUsername` (`window/window_session_controller.cpp:573-621`):
/// - When `referer` is empty/absent and `session.peer_by_username(username)` finds a loaded
///   peer, returns it immediately without issuing `contacts.resolveUsername`.
/// - Otherwise invokes `contacts.resolveUsername`, applies returned `users` and `chats` to
///   `session`, and returns the resolved peer ID.
pub async fn resolve_username<I: Invoker>(
    invoker: &I,
    session: &mut Session,
    username: &str,
    referer: Option<&str>,
) -> Result<ResolvedUsername, ApiError> {
    let trimmed = username.trim();
    let normalized = trimmed.strip_prefix('@').unwrap_or(trimmed);
    let clean_referer = referer.filter(|r| !r.is_empty());

    if clean_referer.is_none()
        && let Some(cached) = session.peer_by_username(normalized)
    {
        return Ok(ResolvedUsername {
            peer_id: cached,
            from_cache: true,
        });
    }

    let resolved = invoker
        .contacts_resolve_username(tl_fns::contacts::ResolveUsername {
            username: normalized.to_owned(),
            referer: clean_referer.map(str::to_owned),
        })
        .await?;
    apply_tl_users_and_chats(session, &resolved.users, &resolved.chats);
    let peer_id = peer_id_from_tl(&resolved.peer);
    Ok(ResolvedUsername {
        peer_id,
        from_cache: false,
    })
}

// ============================================================================
// Blocked peers pagination (`api/api_blocked_peers.cpp`)
// ============================================================================

/// Limit for the first `contacts.getBlocked` request (`kBlockedFirstSlice` in
/// `api/api_blocked_peers.cpp:21`).
pub const BLOCKED_FIRST_SLICE: i32 = 16;

/// Limit for subsequent `contacts.getBlocked` pages (`kBlockedPerPage` in
/// `api/api_blocked_peers.cpp:22`).
pub const BLOCKED_PER_PAGE: i32 = 40;

/// Single entry in the blocked peers list (`Api::BlockedPeers::Slice::Item`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockedPeerEntry {
    /// Blocked peer ID.
    pub peer_id: PeerId,
    /// Unix timestamp when the peer was blocked.
    pub date: TimeId,
}

/// Slice returned by `contacts.getBlocked` (`Api::BlockedPeers::Slice`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockedPeersSlice {
    /// Blocked peer entries in this slice (or accumulated list).
    pub list: Vec<BlockedPeerEntry>,
    /// Total blocked peer count (`max(count, list.len())`).
    pub total: i32,
}

/// Manages `contacts.getBlocked` pagination, `contacts.block`/`unblock`, and
/// `updatePeerBlocked` reconciliation (`Api::BlockedPeers` in `api/api_blocked_peers.cpp`).
#[derive(Debug, Default, Clone)]
pub struct BlockedPeersManager {
    slice: Option<BlockedPeersSlice>,
}

impl BlockedPeersManager {
    /// Creates a new `BlockedPeersManager`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the currently cached blocked-peers slice, if loaded.
    #[must_use]
    pub fn slice(&self) -> Option<&BlockedPeersSlice> {
        self.slice.as_ref()
    }

    /// Returns whether all blocked peers have been fetched (`list.len() >= total`).
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.slice
            .as_ref()
            .is_some_and(|s| (s.list.len() as i32) >= s.total)
    }

    /// Requests one page of blocked peers at `offset` (`Api::BlockedPeers::request` in
    /// `api/api_blocked_peers.cpp:210-224`), using `limit = 16` when `offset == 0` and
    /// `limit = 40` for `offset > 0`.
    ///
    /// Applies returned `users` and `chats` to `session`, marks those peers blocked in
    /// `session`, and merges the page into the cached slice.
    pub async fn load_page<I: Invoker>(
        &mut self,
        invoker: &I,
        session: &mut Session,
        offset: i32,
    ) -> Result<BlockedPeersSlice, ApiError> {
        let limit = if offset == 0 {
            BLOCKED_FIRST_SLICE
        } else {
            BLOCKED_PER_PAGE
        };
        let resp = invoker
            .contacts_get_blocked(tl_fns::contacts::GetBlocked {
                my_stories_from: false,
                offset,
                limit,
            })
            .await?;

        let (count, blocked_tl, users, chats) = match resp {
            tl_enums::contacts::Blocked::Slice(s) => (s.count, s.blocked, s.users, s.chats),
            tl_enums::contacts::Blocked::Blocked(b) => (0, b.blocked, b.users, b.chats),
        };

        apply_tl_users_and_chats(session, &users, &chats);

        let mut list = Vec::with_capacity(blocked_tl.len());
        for item in blocked_tl {
            let tl_enums::PeerBlocked::PeerBlocked(pb) = item;
            let peer_id = peer_id_from_tl(&pb.peer_id);
            if !peer_id.is_zero() {
                session.set_peer_blocked(peer_id, true);
                list.push(BlockedPeerEntry {
                    peer_id,
                    date: pb.date,
                });
            }
        }
        let total = count.max(list.len() as i32);
        let page = BlockedPeersSlice { list, total };

        if offset == 0 {
            self.slice = Some(page.clone());
        } else if let Some(cached) = self.slice.as_mut() {
            for entry in &page.list {
                if !cached.list.iter().any(|e| e.peer_id == entry.peer_id) {
                    cached.list.push(*entry);
                }
            }
            cached.total = page.total.max(cached.list.len() as i32);
        } else {
            self.slice = Some(page.clone());
        }

        Ok(page)
    }

    /// Blocks `peer_id` via `contacts.block` (`Api::BlockedPeers::block` in
    /// `api/api_blocked_peers.cpp:75-114`).
    pub async fn block<I: Invoker>(
        &mut self,
        invoker: &I,
        session: &mut Session,
        peer_id: PeerId,
        now: TimeId,
    ) -> Result<bool, ApiError> {
        let id = input_peer_for(session, peer_id);
        let ok = invoker
            .contacts_block(tl_fns::contacts::Block {
                my_stories_from: false,
                id,
            })
            .await?;
        if ok {
            self.apply_blocked_change(session, peer_id, true, now);
        }
        Ok(ok)
    }

    /// Unblocks `peer_id` via `contacts.unblock` (`Api::BlockedPeers::unblock` in
    /// `api/api_blocked_peers.cpp:116-166`).
    pub async fn unblock<I: Invoker>(
        &mut self,
        invoker: &I,
        session: &mut Session,
        peer_id: PeerId,
    ) -> Result<bool, ApiError> {
        let id = input_peer_for(session, peer_id);
        let ok = invoker
            .contacts_unblock(tl_fns::contacts::Unblock {
                my_stories_from: false,
                id,
            })
            .await?;
        if ok {
            self.apply_blocked_change(session, peer_id, false, 0);
        }
        Ok(ok)
    }

    /// Reconciles an incoming `updatePeerBlocked` (or local block/unblock RPC result)
    /// into both `session` and the cached blocked-peers slice.
    pub fn apply_blocked_change(
        &mut self,
        session: &mut Session,
        peer_id: PeerId,
        blocked: bool,
        now: TimeId,
    ) {
        session.set_peer_blocked(peer_id, blocked);
        if let Some(slice) = self.slice.as_mut() {
            if blocked {
                if !slice.list.iter().any(|e| e.peer_id == peer_id) {
                    slice
                        .list
                        .insert(0, BlockedPeerEntry { peer_id, date: now });
                    slice.total += 1;
                }
            } else {
                let before = slice.list.len();
                slice.list.retain(|e| e.peer_id != peer_id);
                if slice.list.len() < before && slice.total > slice.list.len() as i32 {
                    slice.total -= 1;
                }
            }
        }
    }
}

// ============================================================================
// User Privacy (`api/api_user_privacy.h`, `api/api_user_privacy.cpp`)
// ============================================================================

/// Privacy setting key (`Api::UserPrivacy::Key` in `api/api_user_privacy.h:26-41`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrivacyKey {
    /// Who can see my phone number (`privacyKeyPhoneNumber`).
    PhoneNumber,
    /// Who can find me by phone number (`privacyKeyAddedByPhone`).
    AddedByPhone,
    /// Who can see my last-seen / online status (`privacyKeyStatusTimestamp`).
    LastSeen,
    /// Who can call me (`privacyKeyPhoneCall`).
    Calls,
    /// Who can invite me to groups/channels (`privacyKeyChatInvite`).
    Invites,
    /// Who can use P2P in calls (`privacyKeyPhoneP2P`).
    CallsPeer2Peer,
    /// Who can add a link to my account when forwarding messages (`privacyKeyForwards`).
    Forwards,
    /// Who can see my profile photo (`privacyKeyProfilePhoto`).
    ProfilePhoto,
    /// Who can send me voice/video messages (`privacyKeyVoiceMessages`).
    Voices,
    /// Who can see my bio (`privacyKeyAbout`).
    About,
    /// Who can see my birthday (`privacyKeyBirthday`).
    Birthday,
    /// Who can send gifts that auto-save to my profile (`privacyKeyStarGiftsAutoSave`).
    GiftsAutoSave,
    /// Who can message me without paying Stars (`privacyKeyNoPaidMessages`).
    NoPaidMessages,
    /// Who can see my saved profile music (`privacyKeySavedMusic`).
    SavedMusic,
}

/// Primary privacy audience (`Api::UserPrivacy::Option` in `api/api_user_privacy.h:42-47`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum PrivacyOption {
    /// Everyone (`privacyValueAllowAll`).
    #[default]
    Everyone,
    /// My contacts (`privacyValueAllowContacts`).
    Contacts,
    /// Close friends (`privacyValueAllowCloseFriends`).
    CloseFriends,
    /// Nobody (`privacyValueDisallowAll`).
    Nobody,
}

/// Allow or disallow exception list (`Api::UserPrivacy::Exceptions` in `api/api_user_privacy.h:48-52`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrivacyExceptions {
    /// Exception user, basic chat, and channel peers.
    pub peers: Vec<PeerId>,
    /// Include Telegram Premium users (`privacyValueAllowPremium`).
    pub premiums: bool,
    /// Include/exclude mini-apps / bots (`privacyValueAllowBots` / `privacyValueDisallowBots`).
    pub miniapps: bool,
}

/// Complete privacy rule (`Api::UserPrivacy::Rule` in `api/api_user_privacy.h:53-59`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrivacyRule {
    /// Base audience.
    pub option: PrivacyOption,
    /// Always-allow exceptions.
    pub always: PrivacyExceptions,
    /// Never-allow exceptions.
    pub never: PrivacyExceptions,
    /// Suppress `always` rules when serializing (`Rule::ignoreAlways`).
    pub ignore_always: bool,
    /// Suppress `never` rules when serializing (`Rule::ignoreNever`).
    pub ignore_never: bool,
}

/// Converts a [`PrivacyKey`] into a TL `InputPrivacyKey` (`KeyToTL` in
/// `api/api_user_privacy.cpp:108-127`).
#[must_use]
pub const fn privacy_key_to_tl(key: PrivacyKey) -> tl_enums::InputPrivacyKey {
    match key {
        PrivacyKey::Calls => tl_enums::InputPrivacyKey::PhoneCall,
        PrivacyKey::Invites => tl_enums::InputPrivacyKey::ChatInvite,
        PrivacyKey::PhoneNumber => tl_enums::InputPrivacyKey::PhoneNumber,
        PrivacyKey::AddedByPhone => tl_enums::InputPrivacyKey::AddedByPhone,
        PrivacyKey::LastSeen => tl_enums::InputPrivacyKey::StatusTimestamp,
        PrivacyKey::CallsPeer2Peer => tl_enums::InputPrivacyKey::PhoneP2P,
        PrivacyKey::Forwards => tl_enums::InputPrivacyKey::Forwards,
        PrivacyKey::ProfilePhoto => tl_enums::InputPrivacyKey::ProfilePhoto,
        PrivacyKey::Voices => tl_enums::InputPrivacyKey::VoiceMessages,
        PrivacyKey::About => tl_enums::InputPrivacyKey::About,
        PrivacyKey::Birthday => tl_enums::InputPrivacyKey::Birthday,
        PrivacyKey::GiftsAutoSave => tl_enums::InputPrivacyKey::StarGiftsAutoSave,
        PrivacyKey::NoPaidMessages => tl_enums::InputPrivacyKey::NoPaidMessages,
        PrivacyKey::SavedMusic => tl_enums::InputPrivacyKey::SavedMusic,
    }
}

/// Converts a TL `PrivacyKey` (from `updatePrivacy`) into [`PrivacyKey`] (`TLToKey` in
/// `api/api_user_privacy.cpp:129-162`).
#[must_use]
pub const fn privacy_key_from_tl(key: &tl_enums::PrivacyKey) -> Option<PrivacyKey> {
    match key {
        tl_enums::PrivacyKey::PhoneNumber => Some(PrivacyKey::PhoneNumber),
        tl_enums::PrivacyKey::AddedByPhone => Some(PrivacyKey::AddedByPhone),
        tl_enums::PrivacyKey::StatusTimestamp => Some(PrivacyKey::LastSeen),
        tl_enums::PrivacyKey::ChatInvite => Some(PrivacyKey::Invites),
        tl_enums::PrivacyKey::PhoneCall => Some(PrivacyKey::Calls),
        tl_enums::PrivacyKey::PhoneP2P => Some(PrivacyKey::CallsPeer2Peer),
        tl_enums::PrivacyKey::Forwards => Some(PrivacyKey::Forwards),
        tl_enums::PrivacyKey::ProfilePhoto => Some(PrivacyKey::ProfilePhoto),
        tl_enums::PrivacyKey::VoiceMessages => Some(PrivacyKey::Voices),
        tl_enums::PrivacyKey::About => Some(PrivacyKey::About),
        tl_enums::PrivacyKey::Birthday => Some(PrivacyKey::Birthday),
        tl_enums::PrivacyKey::StarGiftsAutoSave => Some(PrivacyKey::GiftsAutoSave),
        tl_enums::PrivacyKey::NoPaidMessages => Some(PrivacyKey::NoPaidMessages),
        tl_enums::PrivacyKey::SavedMusic => Some(PrivacyKey::SavedMusic),
    }
}

/// Converts TL `PrivacyRule` vector into a domain [`PrivacyRule`], matching
/// `Api::PrivacyRulesFromTL` (`api/api_user_privacy.cpp:255-341`):
/// - First base option encountered wins; if none is present, defaults to `Nobody`.
/// - First occurrence of a peer in `always` vs `never` wins (no duplicates, no peer in both).
/// - Chat/channel participant exceptions resolve via `session.chat_loaded(id)` then
///   `session.channel_loaded(id)`.
#[must_use]
pub fn privacy_rules_from_tl(rules: &[tl_enums::PrivacyRule], session: &Session) -> PrivacyRule {
    let mut result = PrivacyRule::default();
    let mut option_set = false;
    let mut set_option = |opt: PrivacyOption| {
        if !option_set {
            option_set = true;
            result.option = opt;
        }
    };

    for rule in rules {
        match rule {
            tl_enums::PrivacyRule::PrivacyValueAllowAll => set_option(PrivacyOption::Everyone),
            tl_enums::PrivacyRule::PrivacyValueAllowContacts => {
                set_option(PrivacyOption::Contacts);
            }
            tl_enums::PrivacyRule::PrivacyValueAllowCloseFriends => {
                set_option(PrivacyOption::CloseFriends);
            }
            tl_enums::PrivacyRule::PrivacyValueDisallowAll => set_option(PrivacyOption::Nobody),
            tl_enums::PrivacyRule::PrivacyValueAllowPremium => result.always.premiums = true,
            tl_enums::PrivacyRule::PrivacyValueAllowBots => result.always.miniapps = true,
            tl_enums::PrivacyRule::PrivacyValueDisallowBots => result.never.miniapps = true,
            tl_enums::PrivacyRule::PrivacyValueDisallowContacts => {}
            tl_enums::PrivacyRule::PrivacyValueAllowUsers(data) => {
                for &uid in &data.users {
                    let pid = PeerId::from_user(UserId::from_tl(uid));
                    if !result.never.peers.contains(&pid) && !result.always.peers.contains(&pid) {
                        result.always.peers.push(pid);
                    }
                }
            }
            tl_enums::PrivacyRule::PrivacyValueDisallowUsers(data) => {
                for &uid in &data.users {
                    let pid = PeerId::from_user(UserId::from_tl(uid));
                    if !result.always.peers.contains(&pid) && !result.never.peers.contains(&pid) {
                        result.never.peers.push(pid);
                    }
                }
            }
            tl_enums::PrivacyRule::PrivacyValueAllowChatParticipants(data) => {
                for &cid in &data.chats {
                    let peer = resolve_privacy_chat_peer(session, cid);
                    if let Some(pid) = peer
                        && !result.never.peers.contains(&pid)
                        && !result.always.peers.contains(&pid)
                    {
                        result.always.peers.push(pid);
                    }
                }
            }
            tl_enums::PrivacyRule::PrivacyValueDisallowChatParticipants(data) => {
                for &cid in &data.chats {
                    let peer = resolve_privacy_chat_peer(session, cid);
                    if let Some(pid) = peer
                        && !result.always.peers.contains(&pid)
                        && !result.never.peers.contains(&pid)
                    {
                        result.never.peers.push(pid);
                    }
                }
            }
        }
    }
    // Implicit trailing `privacyValueDisallowAll` (`api/api_user_privacy.cpp:339`).
    set_option(PrivacyOption::Nobody);
    result
}

/// Converts a domain [`PrivacyRule`] into TL `InputPrivacyRule`s in exact tdesktop order
/// (`RulesToTL` in `api/api_user_privacy.cpp:28-106`):
/// 1. `always` (unless `ignore_always`): `AllowUsers`, `AllowChatParticipants`, `AllowPremium`
///    (when `option != Everyone`), `AllowBots` (when `option != Everyone`).
/// 2. `never` (unless `ignore_never`): `DisallowUsers`, `DisallowChatParticipants`,
///    `DisallowBots` (when `option != Nobody`).
/// 3. Base option rule (`AllowAll` / `AllowContacts` / `AllowCloseFriends` / `DisallowAll`).
#[must_use]
pub fn privacy_rules_to_tl(
    rule: &PrivacyRule,
    session: &Session,
) -> Vec<tl_enums::InputPrivacyRule> {
    let collect_users = |exc: &PrivacyExceptions| -> Vec<tl_enums::InputUser> {
        exc.peers
            .iter()
            .filter_map(|pid| pid.to_user())
            .map(|uid| input_user_for(session, uid))
            .collect()
    };
    let collect_chats = |exc: &PrivacyExceptions| -> Vec<i64> {
        exc.peers
            .iter()
            .filter(|pid| !pid.is_user())
            .map(|pid| pid.bare() as i64)
            .collect()
    };

    let mut out = Vec::new();
    if !rule.ignore_always {
        let users = collect_users(&rule.always);
        let chats = collect_chats(&rule.always);
        if !users.is_empty() {
            out.push(tl_enums::InputPrivacyRule::InputPrivacyValueAllowUsers(
                tl_types::InputPrivacyValueAllowUsers { users },
            ));
        }
        if !chats.is_empty() {
            out.push(
                tl_enums::InputPrivacyRule::InputPrivacyValueAllowChatParticipants(
                    tl_types::InputPrivacyValueAllowChatParticipants { chats },
                ),
            );
        }
        if rule.always.premiums && rule.option != PrivacyOption::Everyone {
            out.push(tl_enums::InputPrivacyRule::InputPrivacyValueAllowPremium);
        }
        if rule.always.miniapps && rule.option != PrivacyOption::Everyone {
            out.push(tl_enums::InputPrivacyRule::InputPrivacyValueAllowBots);
        }
    }
    if !rule.ignore_never {
        let users = collect_users(&rule.never);
        let chats = collect_chats(&rule.never);
        if !users.is_empty() {
            out.push(tl_enums::InputPrivacyRule::InputPrivacyValueDisallowUsers(
                tl_types::InputPrivacyValueDisallowUsers { users },
            ));
        }
        if !chats.is_empty() {
            out.push(
                tl_enums::InputPrivacyRule::InputPrivacyValueDisallowChatParticipants(
                    tl_types::InputPrivacyValueDisallowChatParticipants { chats },
                ),
            );
        }
        if rule.never.miniapps && rule.option != PrivacyOption::Nobody {
            out.push(tl_enums::InputPrivacyRule::InputPrivacyValueDisallowBots);
        }
    }
    out.push(match rule.option {
        PrivacyOption::Everyone => tl_enums::InputPrivacyRule::InputPrivacyValueAllowAll,
        PrivacyOption::Contacts => tl_enums::InputPrivacyRule::InputPrivacyValueAllowContacts,
        PrivacyOption::CloseFriends => {
            tl_enums::InputPrivacyRule::InputPrivacyValueAllowCloseFriends
        }
        PrivacyOption::Nobody => tl_enums::InputPrivacyRule::InputPrivacyValueDisallowAll,
    });
    out
}

/// Returns `true` if every chat/channel referenced by `AllowChatParticipants` or
/// `DisallowChatParticipants` in `rules` is already loaded in `session`
/// (`api/api_updates.cpp:2402-2424`, `allLoaded()`).
#[must_use]
pub fn privacy_rules_all_chats_loaded(rules: &[tl_enums::PrivacyRule], session: &Session) -> bool {
    for rule in rules {
        let chats = match rule {
            tl_enums::PrivacyRule::PrivacyValueAllowChatParticipants(d) => &d.chats[..],
            tl_enums::PrivacyRule::PrivacyValueDisallowChatParticipants(d) => &d.chats[..],
            _ => continue,
        };
        for &cid in chats {
            if resolve_privacy_chat_peer(session, cid).is_none() {
                return false;
            }
        }
    }
    true
}

fn resolve_privacy_chat_peer(session: &Session, bare_id: i64) -> Option<PeerId> {
    let chat_id = ChatId::from_tl(bare_id);
    if session.chat_loaded(chat_id) {
        return Some(PeerId::from_chat(chat_id));
    }
    let channel_id = ChannelId::from_tl(bare_id);
    if session.channel_loaded(channel_id) {
        return Some(PeerId::from_channel(channel_id));
    }
    None
}

/// Stateful account privacy rules manager (`Api::UserPrivacy` in `api/api_user_privacy.cpp`).
#[derive(Debug, Default, Clone)]
pub struct UserPrivacyManager {
    values: HashMap<PrivacyKey, PrivacyRule>,
}

impl UserPrivacyManager {
    /// Creates a new `UserPrivacyManager`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the cached [`PrivacyRule`] for `key`, if loaded.
    #[must_use]
    pub fn value(&self, key: PrivacyKey) -> Option<&PrivacyRule> {
        self.values.get(&key)
    }

    /// Fetches privacy rules for `key` via `account.getPrivacy` (`UserPrivacy::reload` in
    /// `api/api_user_privacy.cpp:219-236`).
    pub async fn reload<I: Invoker>(
        &mut self,
        invoker: &I,
        session: &mut Session,
        key: PrivacyKey,
    ) -> Result<PrivacyRule, ApiError> {
        let resp = invoker
            .account_get_privacy(tl_fns::account::GetPrivacy {
                key: privacy_key_to_tl(key),
            })
            .await?;
        apply_tl_users_and_chats(session, &resp.users, &resp.chats);
        let parsed = privacy_rules_from_tl(&resp.rules, session);
        self.values.insert(key, parsed.clone());
        Ok(parsed)
    }

    /// Saves privacy rules for `key` via `account.setPrivacy` (`UserPrivacy::save` in
    /// `api/api_user_privacy.cpp:171-201`).
    pub async fn save<I: Invoker>(
        &mut self,
        invoker: &I,
        session: &mut Session,
        key: PrivacyKey,
        rule: &PrivacyRule,
    ) -> Result<PrivacyRule, ApiError> {
        let resp = invoker
            .account_set_privacy(tl_fns::account::SetPrivacy {
                key: privacy_key_to_tl(key),
                rules: privacy_rules_to_tl(rule, session),
            })
            .await?;
        apply_tl_users_and_chats(session, &resp.users, &resp.chats);
        let parsed = privacy_rules_from_tl(&resp.rules, session);
        self.values.insert(key, parsed.clone());
        Ok(parsed)
    }

    /// Applies an incoming `updatePrivacy` (`UserPrivacy::apply` in
    /// `api/api_user_privacy.cpp:203-217`, `api/api_updates.cpp:2400-2429`):
    /// if all chat/channel exception peers are already loaded in `session`, parses directly;
    /// otherwise reloads from the server via `account.getPrivacy`.
    pub async fn apply_update<I: Invoker>(
        &mut self,
        invoker: &I,
        session: &mut Session,
        key_tl: &tl_enums::PrivacyKey,
        rules_tl: &[tl_enums::PrivacyRule],
    ) -> Result<Option<(PrivacyKey, PrivacyRule)>, ApiError> {
        let Some(key) = privacy_key_from_tl(key_tl) else {
            return Ok(None);
        };
        if !privacy_rules_all_chats_loaded(rules_tl, session) {
            let reloaded = self.reload(invoker, session, key).await?;
            return Ok(Some((key, reloaded)));
        }
        let parsed = privacy_rules_from_tl(rules_tl, session);
        self.values.insert(key, parsed.clone());
        Ok(Some((key, parsed)))
    }
}
