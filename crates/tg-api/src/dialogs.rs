//! Dialogs pagination, pinned dialogs, folders/archive, and chat filters loading.
//!
//! Port origin: `apiwrap.cpp` (`requestDialogs`, `requestMoreDialogs`,
//! `updateDialogsOffset`, `requestPinnedDialogs`, `savePinnedOrder`,
//! `toggleHistoryArchived`), `data/data_chat_filters.cpp` (`load`, `loadNextExceptions`).

use std::collections::{HashMap, HashSet, VecDeque};

use tg_data::{
    ARCHIVE_FOLDER_ID, ChannelId, ChatId, FilterId, FolderId, MsgId, PeerId, Session, TimeId,
    UserId,
};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::convert::{
    apply_tl_dialog_in_folder, apply_tl_message, apply_tl_peer_dialogs_for_folder,
    apply_tl_users_and_chats, dialog_filter_from_tl, peer_id_from_input_peer, peer_id_from_tl,
    tl_message_meta,
};
use crate::error::ApiError;
use crate::invoker::Invoker;

/// Limit for the very first `messages.getDialogs` page (`kDialogsFirstLoad`).
pub const DIALOGS_FIRST_LOAD: i32 = 20;

/// Limit for subsequent `messages.getDialogs` pages (`kDialogsPerPage`).
pub const DIALOGS_PER_PAGE: i32 = 500;

/// Minimum dialogs count before loading filter exception peers (`kLoadExceptionsAfter`).
pub const LOAD_EXCEPTIONS_AFTER: usize = 100;

/// Maximum peers per `messages.getPeerDialogs` filter exception batch (`kLoadExceptionsPerRequest`).
pub const LOAD_EXCEPTIONS_PER_REQUEST: usize = 100;

/// Pagination state for one folder (`0` = main list, `1` = archive) (`ApiWrap::DialogsLoadState`).
#[derive(Debug, Clone, PartialEq)]
pub struct FolderDialogsState {
    /// Last dialog's top message date (`0` on first load).
    pub offset_date: TimeId,
    /// Last dialog's top message ID (`0` on first load).
    pub offset_id: MsgId,
    /// Last dialog's peer (`InputPeer::Empty` on first load).
    pub offset_peer: tl_enums::InputPeer,
    /// Whether all unpinned dialog slices have been received.
    pub list_received: bool,
    /// Whether pinned dialogs for this folder have been received.
    pub pinned_received: bool,
    /// Whether a `messages.getDialogs` request is currently in flight.
    pub request_in_flight: bool,
    /// Whether a `messages.getPinnedDialogs` request is currently in flight.
    pub pinned_in_flight: bool,
    /// Total count reported by server (if known).
    pub total_count: Option<i32>,
}

impl Default for FolderDialogsState {
    fn default() -> Self {
        Self {
            offset_date: 0,
            offset_id: MsgId::ZERO,
            offset_peer: tl_enums::InputPeer::Empty,
            list_received: false,
            pinned_received: false,
            request_in_flight: false,
            pinned_in_flight: false,
            total_count: None,
        }
    }
}

impl FolderDialogsState {
    /// Returns `true` when both unpinned pages and pinned dialogs have finished loading.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.list_received && self.pinned_received
    }
}

/// Output of a single `DialogsLoader::load_next_page` call.
#[derive(Debug, Clone, PartialEq)]
pub struct DialogsPageResult {
    /// Folder ID (`0` = main, `1` = archive).
    pub folder_id: FolderId,
    /// Dialogs returned by `messages.getDialogs`.
    pub dialogs: Vec<tl_enums::Dialog>,
    /// Associated messages returned by `messages.getDialogs`.
    pub messages: Vec<tl_enums::Message>,
    /// Users returned by `messages.getDialogs`.
    pub users: Vec<tl_enums::User>,
    /// Chats/channels returned by `messages.getDialogs`.
    pub chats: Vec<tl_enums::Chat>,
    /// Pinned dialogs fetched alongside the first page (if requested).
    pub pinned: Option<tl_types::messages::PeerDialogs>,
    /// Total dialog count reported by server.
    pub total_count: i32,
    /// Whether the folder is now completely loaded (both list and pinned).
    pub folder_done: bool,
}

impl DialogsPageResult {
    /// Applies all users, chats, messages, dialogs, and pinned dialogs from this page
    /// directly into `session`, matching `ApiWrap::requestMoreDialogs` (`apiwrap.cpp:1009-1015`),
    /// `ApiWrap::sendPinnedDialogsRequest` (`apiwrap.cpp:1176-1183`), and
    /// `Session::applyDialogs` (`data/data_session.cpp:2845-2859`).
    pub fn apply_to_session(&self, session: &mut Session, now: TimeId) {
        let request_folder = (self.folder_id != 0).then_some(self.folder_id);
        if let Some(pinned) = &self.pinned {
            // Clear stale pins for this folder before applying the server's pinned order
            // (`apiwrap.cpp:1178`, `clearPinnedChats(folder)`).
            apply_tl_peer_dialogs_for_folder(session, pinned, request_folder, true, now);
        }
        apply_tl_users_and_chats(session, &self.users, &self.chats);
        for msg in &self.messages {
            apply_tl_message(session, msg);
        }
        for dialog in &self.dialogs {
            apply_tl_dialog_in_folder(session, dialog, request_folder, now);
        }
        if self.total_count >= 0 {
            session.set_folder_cloud_list_size(self.folder_id, self.total_count as usize);
        }
        if self.folder_done {
            session.set_folder_loaded(self.folder_id, true);
        }
    }
}

/// Stateful dialogs, archive, pinned order, and chat filters loader.
#[derive(Debug, Default)]
pub struct DialogsLoader {
    folders: HashMap<FolderId, FolderDialogsState>,
    peer_inputs: HashMap<PeerId, tl_enums::InputPeer>,
    loaded_dialogs_count: usize,
    filters: Vec<tl_enums::DialogFilter>,
    tags_enabled: bool,
    filters_loaded: bool,
    exception_filter_queue: VecDeque<FilterId>,
    known_folder_peers: HashSet<PeerId>,
}

impl DialogsLoader {
    /// Creates a new `DialogsLoader` initialized for the main dialog list (`folder_id = 0`).
    #[must_use]
    pub fn new() -> Self {
        let mut loader = Self::default();
        loader.folders.insert(0, FolderDialogsState::default());
        loader
    }

    /// Registers an `InputPeer` so `updateDialogsOffset` can build the next page's `offset_peer`.
    pub fn register_peer_input(&mut self, peer: tl_enums::InputPeer) {
        let id = peer_id_from_input_peer(&peer, None);
        if !id.is_zero() {
            self.peer_inputs.insert(id, peer);
        }
    }

    /// Extracts and registers `InputPeer` access hashes from TL `users` and `chats`.
    ///
    /// Never overwrites an already-known `InputPeer` with a `min` constructor
    /// (`data/data_session.cpp:828-832, 1076-1080`) and uses `InputPeer::Self_` for the
    /// logged-in user (`data/data_peer.cpp:2090-2092`).
    pub fn register_peers_from_tl(&mut self, users: &[tl_enums::User], chats: &[tl_enums::Chat]) {
        for user in users {
            if let tl_enums::User::User(u) = user {
                let pid = PeerId::from_user(UserId::from_tl(u.id));
                if u.self_ {
                    self.peer_inputs.insert(pid, tl_enums::InputPeer::Self_);
                } else if let Some(hash) = u.access_hash {
                    if u.min && self.peer_inputs.contains_key(&pid) {
                        continue;
                    }
                    self.peer_inputs.insert(
                        pid,
                        tl_enums::InputPeer::User(tl_types::InputPeerUser {
                            user_id: u.id,
                            access_hash: hash,
                        }),
                    );
                }
            }
        }
        for chat in chats {
            match chat {
                tl_enums::Chat::Chat(c) => {
                    let pid = PeerId::from_chat(ChatId::from_tl(c.id));
                    self.peer_inputs.insert(
                        pid,
                        tl_enums::InputPeer::Chat(tl_types::InputPeerChat { chat_id: c.id }),
                    );
                }
                tl_enums::Chat::Forbidden(c) => {
                    let pid = PeerId::from_chat(ChatId::from_tl(c.id));
                    self.peer_inputs.insert(
                        pid,
                        tl_enums::InputPeer::Chat(tl_types::InputPeerChat { chat_id: c.id }),
                    );
                }
                tl_enums::Chat::Channel(ch) => {
                    if let Some(hash) = ch.access_hash {
                        let pid = PeerId::from_channel(ChannelId::from_tl(ch.id));
                        if ch.min && self.peer_inputs.contains_key(&pid) {
                            continue;
                        }
                        self.peer_inputs.insert(
                            pid,
                            tl_enums::InputPeer::Channel(tl_types::InputPeerChannel {
                                channel_id: ch.id,
                                access_hash: hash,
                            }),
                        );
                    }
                }
                tl_enums::Chat::ChannelForbidden(ch) => {
                    let pid = PeerId::from_channel(ChannelId::from_tl(ch.id));
                    self.peer_inputs.insert(
                        pid,
                        tl_enums::InputPeer::Channel(tl_types::InputPeerChannel {
                            channel_id: ch.id,
                            access_hash: ch.access_hash,
                        }),
                    );
                }
                _ => {}
            }
        }
    }

    /// Marks a peer's folder as known locally (used to skip redundant filter-exception fetches).
    pub fn mark_peer_folder_known(&mut self, peer_id: PeerId) {
        self.known_folder_peers.insert(peer_id);
    }

    /// Resets pagination state for `folder_id` so the next `load_next_page` call refetches
    /// from the top (e.g. after `updates.differenceTooLong`).
    pub fn reset_folder(&mut self, folder_id: FolderId) {
        self.folders
            .insert(folder_id, FolderDialogsState::default());
        if folder_id == 0 {
            self.loaded_dialogs_count = 0;
        }
    }

    /// Returns the pagination state for `folder_id`, if initialized.
    #[must_use]
    pub fn folder_state(&self, folder_id: FolderId) -> Option<&FolderDialogsState> {
        self.folders.get(&folder_id)
    }

    /// Returns `true` if `folder_id` has completed both list and pinned loading.
    #[must_use]
    pub fn is_folder_done(&self, folder_id: FolderId) -> bool {
        self.folders
            .get(&folder_id)
            .is_some_and(FolderDialogsState::is_complete)
    }

    /// Returns `true` if any loaded chat filter includes archived chats
    /// (`ChatFilters::archiveNeeded`).
    #[must_use]
    pub fn archive_needed(&self) -> bool {
        self.filters.iter().any(|f| match f {
            tl_enums::DialogFilter::DialogFilter(df) => !df.exclude_archived,
            tl_enums::DialogFilter::Chatlist(_) => true,
            tl_enums::DialogFilter::Default => false,
        })
    }

    /// Returns the currently loaded TL dialog filters.
    #[must_use]
    pub fn filters(&self) -> &[tl_enums::DialogFilter] {
        &self.filters
    }

    /// Returns whether folder tags are enabled.
    #[must_use]
    pub const fn tags_enabled(&self) -> bool {
        self.tags_enabled
    }

    /// Loads the next page of dialogs for `folder_id` (`0` = main list, `1` = archive)
    /// (`ApiWrap::requestDialogs` / `requestMoreDialogs`).
    ///
    /// Uses `limit = 20` on the first load (`offset_date == 0`) and `limit = 500` on
    /// subsequent pages, always setting `exclude_pinned = true` and `folder_id = Some(folder_id)`.
    /// Also fetches `messages.getPinnedDialogs(folder_id)` if pinned dialogs have not
    /// yet been loaded for that folder.
    ///
    /// Returns `Ok(None)` if the folder is already finished or a request is already in flight.
    pub async fn load_next_page<I: Invoker>(
        &mut self,
        invoker: &I,
        folder_id: FolderId,
    ) -> Result<Option<DialogsPageResult>, ApiError> {
        let state = self.folders.entry(folder_id).or_default();
        if state.list_received || state.request_in_flight {
            return Ok(None);
        }

        let first_load = state.offset_date == 0;
        let limit = if first_load {
            DIALOGS_FIRST_LOAD
        } else {
            DIALOGS_PER_PAGE
        };
        let req = tl_fns::messages::GetDialogs {
            exclude_pinned: true,
            folder_id: Some(folder_id),
            offset_date: state.offset_date,
            offset_id: state.offset_id.as_i32().unwrap_or(0),
            offset_peer: state.offset_peer.clone(),
            limit,
            hash: 0,
        };
        state.request_in_flight = true;

        let need_pinned = !state.pinned_received && !state.pinned_in_flight;
        if need_pinned {
            state.pinned_in_flight = true;
        }

        let response = match invoker.messages_get_dialogs(req).await {
            Ok(res) => res,
            Err(err) => {
                if let Some(st) = self.folders.get_mut(&folder_id) {
                    st.request_in_flight = false;
                    if need_pinned {
                        st.pinned_in_flight = false;
                    }
                }
                return Err(ApiError::Invocation(err));
            }
        };

        let pinned = if need_pinned {
            match invoker
                .messages_get_pinned_dialogs(tl_fns::messages::GetPinnedDialogs { folder_id })
                .await
            {
                Ok(p) => {
                    self.register_peers_from_tl(&p.users, &p.chats);
                    for d in &p.dialogs {
                        if let Some(pid) = dialog_peer_id(d) {
                            self.known_folder_peers.insert(pid);
                        }
                    }
                    if let Some(st) = self.folders.get_mut(&folder_id) {
                        st.pinned_in_flight = false;
                        st.pinned_received = true;
                    }
                    Some(p)
                }
                Err(_) => {
                    // Matches apiwrap.cpp:1186: fail handler still finishes pinned state.
                    if let Some(st) = self.folders.get_mut(&folder_id) {
                        st.pinned_in_flight = false;
                        st.pinned_received = true;
                    }
                    None
                }
            }
        } else {
            None
        };

        let (dialogs, messages, users, chats, total_count, finished_list) = match response {
            tl_enums::messages::Dialogs::Dialogs(d) => {
                let count = d.dialogs.len() as i32;
                (d.dialogs, d.messages, d.users, d.chats, count, true)
            }
            tl_enums::messages::Dialogs::Slice(s) => {
                (s.dialogs, s.messages, s.users, s.chats, s.count, false)
            }
            tl_enums::messages::Dialogs::NotModified(nm) => (
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                nm.count,
                true,
            ),
        };

        self.register_peers_from_tl(&users, &chats);
        for d in &dialogs {
            if let Some(pid) = dialog_peer_id(d) {
                self.known_folder_peers.insert(pid);
            }
        }
        if folder_id == 0 {
            self.loaded_dialogs_count += dialogs.len();
        }

        if finished_list {
            if let Some(st) = self.folders.get_mut(&folder_id) {
                st.request_in_flight = false;
                st.list_received = true;
                st.total_count = Some(total_count);
            }
        } else {
            self.update_dialogs_offset(folder_id, &dialogs, &messages, total_count);
        }

        let folder_done = self.is_folder_done(folder_id);
        Ok(Some(DialogsPageResult {
            folder_id,
            dialogs,
            messages,
            users,
            chats,
            pinned,
            total_count,
            folder_done,
        }))
    }

    /// Reloads pinned dialogs for `folder_id` (`ApiWrap::reloadPinnedDialogs`).
    pub async fn reload_pinned_dialogs<I: Invoker>(
        &mut self,
        invoker: &I,
        folder_id: FolderId,
    ) -> Result<tl_types::messages::PeerDialogs, ApiError> {
        let res = invoker
            .messages_get_pinned_dialogs(tl_fns::messages::GetPinnedDialogs { folder_id })
            .await?;
        self.register_peers_from_tl(&res.users, &res.chats);
        for d in &res.dialogs {
            if let Some(pid) = dialog_peer_id(d) {
                self.known_folder_peers.insert(pid);
            }
        }
        if let Some(st) = self.folders.get_mut(&folder_id) {
            st.pinned_received = true;
        }
        Ok(res)
    }

    /// Saves pinned dialog order for `folder_id` (`ApiWrap::savePinnedOrder`, always `force = true`).
    pub async fn save_pinned_order<I: Invoker>(
        &self,
        invoker: &I,
        folder_id: FolderId,
        order: Vec<tl_enums::InputDialogPeer>,
    ) -> Result<bool, ApiError> {
        let ok = invoker
            .messages_reorder_pinned_dialogs(tl_fns::messages::ReorderPinnedDialogs {
                force: true,
                folder_id,
                order,
            })
            .await?;
        Ok(ok)
    }

    /// Archives or unarchives a peer (`ApiWrap::toggleHistoryArchived`).
    pub async fn toggle_history_archived<I: Invoker>(
        &self,
        invoker: &I,
        peer: tl_enums::InputPeer,
        archived: bool,
    ) -> Result<tl_enums::Updates, ApiError> {
        let folder_id = if archived { ARCHIVE_FOLDER_ID } else { 0 };
        let updates = invoker
            .folders_edit_peer_folders(tl_fns::folders::EditPeerFolders {
                folder_peers: vec![tl_enums::InputFolderPeer::InputFolderPeer(
                    tl_types::InputFolderPeer { peer, folder_id },
                )],
            })
            .await?;
        Ok(updates)
    }

    /// Marks or unmarks a dialog as unread (`Data::Histories::changeDialogUnreadMark`).
    pub async fn mark_dialog_unread<I: Invoker>(
        &self,
        invoker: &I,
        peer: tl_enums::InputDialogPeer,
        unread: bool,
    ) -> Result<bool, ApiError> {
        let ok = invoker
            .messages_mark_dialog_unread(tl_fns::messages::MarkDialogUnread {
                unread,
                parent_peer: None,
                peer,
            })
            .await?;
        Ok(ok)
    }

    /// Loads chat filters via `messages.getDialogFilters` (`Data::ChatFilters::load`).
    pub async fn load_dialog_filters<I: Invoker>(
        &mut self,
        invoker: &I,
    ) -> Result<&[tl_enums::DialogFilter], ApiError> {
        let resp = invoker.messages_get_dialog_filters().await?;
        self.apply_dialog_filters(resp);
        Ok(&self.filters)
    }

    /// Applies preloaded dialog filters (e.g. from `LoginStep::Authorized::preloaded_filters`).
    pub fn apply_dialog_filters(&mut self, resp: tl_types::messages::DialogFilters) {
        self.tags_enabled = resp.tags_enabled;
        self.filters = resp.filters;
        self.filters_loaded = true;
        self.exception_filter_queue.clear();
        for filter in &self.filters {
            match filter {
                tl_enums::DialogFilter::DialogFilter(df)
                    if !df.include_peers.is_empty() || !df.pinned_peers.is_empty() =>
                {
                    self.exception_filter_queue.push_back(df.id);
                }
                tl_enums::DialogFilter::Chatlist(cl)
                    if !cl.include_peers.is_empty() || !cl.pinned_peers.is_empty() =>
                {
                    self.exception_filter_queue.push_back(cl.id);
                }
                _ => {}
            }
        }
    }

    /// Applies the loaded filters into `session` (`ChatFilters::received` in
    /// `data/data_chat_filters.cpp:491-525`), removing any filters no longer returned
    /// by the server and applying the exact server order.
    pub fn apply_filters_to_session(&self, session: &mut Session) {
        let self_id = Some(session.self_user_id());
        let parsed: Vec<_> = self
            .filters
            .iter()
            .filter_map(|filter| dialog_filter_from_tl(filter, self_id))
            .collect();
        session.sync_filters_from_list(parsed, self.tags_enabled);
    }

    /// Loads the next batch of unknown filter exception dialogs via `messages.getPeerDialogs`
    /// (`Data::ChatFilters::loadNextExceptions`).
    ///
    /// Waits until either the main dialog list is fully loaded or at least
    /// `LOAD_EXCEPTIONS_AFTER` (100) dialogs are loaded, then batches up to
    /// `LOAD_EXCEPTIONS_PER_REQUEST` (100) peers per call.
    pub async fn load_next_filter_exceptions<I: Invoker>(
        &mut self,
        invoker: &I,
    ) -> Result<Option<tl_types::messages::PeerDialogs>, ApiError> {
        let main_done = self.folders.get(&0).is_some_and(|s| s.list_received);
        if !main_done && self.loaded_dialogs_count < LOAD_EXCEPTIONS_AFTER {
            return Ok(None);
        }

        let mut inputs = Vec::new();
        while let Some(&filter_id) = self.exception_filter_queue.front() {
            let mut filter_inputs = Vec::new();
            if let Some(filter) = self
                .filters
                .iter()
                .find(|f| filter_id_of(f) == Some(filter_id))
            {
                let (include_peers, pinned_peers) = match filter {
                    tl_enums::DialogFilter::DialogFilter(df) => {
                        (&df.include_peers[..], &df.pinned_peers[..])
                    }
                    tl_enums::DialogFilter::Chatlist(cl) => {
                        (&cl.include_peers[..], &cl.pinned_peers[..])
                    }
                    tl_enums::DialogFilter::Default => (&[][..], &[][..]),
                };
                for peer in include_peers.iter().chain(pinned_peers.iter()) {
                    let pid = peer_id_from_input_peer(peer, None);
                    if !pid.is_zero() && !self.known_folder_peers.contains(&pid) {
                        filter_inputs.push(tl_enums::InputDialogPeer::InputDialogPeer(
                            tl_types::InputDialogPeer { peer: peer.clone() },
                        ));
                    }
                }
            }
            if !inputs.is_empty()
                && inputs.len() + filter_inputs.len() > LOAD_EXCEPTIONS_PER_REQUEST
            {
                break;
            }
            self.exception_filter_queue.pop_front();
            inputs.extend(filter_inputs);
            if inputs.len() >= LOAD_EXCEPTIONS_PER_REQUEST {
                break;
            }
        }

        if inputs.is_empty() {
            return Ok(None);
        }

        let resp = invoker
            .messages_get_peer_dialogs(tl_fns::messages::GetPeerDialogs { peers: inputs })
            .await?;
        self.register_peers_from_tl(&resp.users, &resp.chats);
        for d in &resp.dialogs {
            if let Some(pid) = dialog_peer_id(d) {
                self.known_folder_peers.insert(pid);
            }
        }
        Ok(Some(resp))
    }

    /// Computes next pagination offset using tdesktop's exact reverse-scan logic
    /// (`ApiWrap::updateDialogsOffset` in `apiwrap.cpp:1068-1115`).
    fn update_dialogs_offset(
        &mut self,
        folder_id: FolderId,
        dialogs: &[tl_enums::Dialog],
        messages: &[tl_enums::Message],
        total_count: i32,
    ) {
        let mut last_date: TimeId = 0;
        let mut last_peer = PeerId::ZERO;
        let mut last_msg_id = MsgId::ZERO;

        for dialog in dialogs.iter().rev() {
            // Only `MTPDdialog` advances pagination offset; `MTPDdialogFolder` and
            // `MTPDdialogCommunity` are skipped (`ApiWrap::updateDialogsOffset` in
            // `apiwrap.cpp:1075-1103`).
            let (peer, top_message) = match dialog {
                tl_enums::Dialog::Dialog(d) => {
                    (peer_id_from_tl(&d.peer), MsgId(i64::from(d.top_message)))
                }
                tl_enums::Dialog::Folder(_) | tl_enums::Dialog::Community(_) => continue,
            };
            if peer.is_zero() || top_message.is_zero() {
                continue;
            }
            if last_peer.is_zero() {
                last_peer = peer;
            }
            if last_msg_id.is_zero() {
                last_msg_id = top_message;
            }
            for message in messages.iter().rev() {
                if let Some((msg_peer, msg_id, msg_date)) = tl_message_meta(message)
                    && msg_id == top_message
                    && msg_peer == peer
                {
                    if msg_date != 0 {
                        last_date = msg_date;
                    }
                    break;
                }
            }
            if last_date != 0 {
                break;
            }
        }

        if let Some(st) = self.folders.get_mut(&folder_id) {
            st.total_count = Some(total_count);
            st.request_in_flight = false;
            if last_date != 0 {
                st.offset_date = last_date;
                st.offset_id = last_msg_id;
                st.offset_peer = self
                    .peer_inputs
                    .get(&last_peer)
                    .cloned()
                    .unwrap_or(tl_enums::InputPeer::Empty);
            } else {
                st.list_received = true;
            }
        }
    }
}

const fn dialog_peer_id(dialog: &tl_enums::Dialog) -> Option<PeerId> {
    match dialog {
        tl_enums::Dialog::Dialog(d) => Some(peer_id_from_tl(&d.peer)),
        tl_enums::Dialog::Folder(f) => Some(peer_id_from_tl(&f.peer)),
        tl_enums::Dialog::Community(c) => {
            Some(PeerId::from_channel(ChannelId::from_tl(c.community_id)))
        }
    }
}

const fn filter_id_of(filter: &tl_enums::DialogFilter) -> Option<FilterId> {
    match filter {
        tl_enums::DialogFilter::DialogFilter(df) => Some(df.id),
        tl_enums::DialogFilter::Chatlist(cl) => Some(cl.id),
        tl_enums::DialogFilter::Default => None,
    }
}
