//! Live Telegram suggested chat folders (`messages.getSuggestedDialogFilters`)
//! and shareable folder links (`chatlists.*`) for [`LiveSession`].
//!
//! Port origin:
//! - `Telegram/SourceFiles/data/data_chat_filters.cpp`
//! - `Telegram/SourceFiles/boxes/filters/edit_filter_links.cpp`
//! - `Telegram/SourceFiles/boxes/filters/edit_filter_chats_list.cpp`

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use tg_api::RpcClient;
use tg_api::convert::{apply_tl_users_and_chats, input_peer_for};
use tg_data::{
    ChannelId, ChatFilter, ChatFilterFlags, ChatId, PeerId as DataPeerId, Session,
    TextWithEntities as DataText, UserId,
};
use tg_tl::{RemoteCall, enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::bridge::LiveBridgeState;
use crate::convert::{chat_filter_to_settings_folder, from_data_peer, to_data_peer};
use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::session::LiveSession;
use tg_app_core::settings::backend::{
    ChatFolder as SettingsChatFolder, FolderInviteLink, MAX_FOLDER_LINK_TITLE_LEN, SettingsBackend,
    SuggestedFolder,
};
use tg_app_core::stand_in::types::PeerId;

/// Default timeout for folder suggestion & chatlist RPC invocations.
const CHATLISTS_RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// Exported shareable folder link (`ExportedChatlistInvite`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedChatlistLink {
    /// Local dialog filter ID owning this shareable link.
    pub filter_id: i32,
    /// Optional label for this link.
    pub title: String,
    /// Full share URL (e.g. `https://t.me/addlist/<slug>`).
    pub url: String,
    /// Extracted invite slug.
    pub slug: String,
    /// Peers included in this invite link.
    pub peers: Vec<PeerId>,
}

/// Result of checking a shareable folder link (`chatlists.checkChatlistInvite`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatlistInviteInfo {
    /// User already joined this chatlist under `filter_id`.
    Already {
        /// Existing local folder ID.
        filter_id: i32,
        /// Newly added peers in the shared folder not yet joined locally.
        missing_peers: Vec<PeerId>,
        /// Peers already present locally.
        already_peers: Vec<PeerId>,
    },
    /// New shareable folder invitation not yet added locally.
    New {
        /// Folder title.
        title: String,
        /// Whether custom emoji animations in title are disabled.
        title_noanimate: bool,
        /// Optional folder icon emoticon.
        emoticon: Option<String>,
        /// Peers included in the invite.
        peers: Vec<PeerId>,
    },
}

/// Pending peer additions for a joined shareable folder (`chatlists.getChatlistUpdates`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatlistUpdatesInfo {
    /// Folder ID whose updates were fetched.
    pub filter_id: i32,
    /// New peers available to join in this shared folder.
    pub missing_peers: Vec<PeerId>,
}

/// Extracts the bare chatlist slug from a URL (`https://t.me/addlist/<slug>`,
/// `tg://addlist?slug=<slug>`) or returns the trimmed slug verbatim.
#[must_use]
pub fn extract_chatlist_slug(input: &str) -> String {
    let trimmed = input.trim();
    if let Some(idx) = trimmed.find("addlist/") {
        let rest = &trimmed[idx + "addlist/".len()..];
        return rest
            .split(&['?', '#', '/'][..])
            .next()
            .unwrap_or("")
            .trim()
            .to_owned();
    }
    if let Some(idx) = trimmed.find("slug=") {
        let rest = &trimmed[idx + "slug=".len()..];
        return rest
            .split(&['&', '#'][..])
            .next()
            .unwrap_or("")
            .trim()
            .to_owned();
    }
    trimmed.trim_start_matches('/').to_owned()
}

// =============================================================================
// Per-session suggested folder & chatlist link cache
// =============================================================================

#[derive(Default)]
struct LiveChatlistsCache {
    suggested_loaded: bool,
    suggested: Vec<SuggestedFolder>,
    links_loaded_filters: HashMap<i32, bool>,
    links_by_filter: HashMap<i32, Vec<ExportedChatlistLink>>,
}

struct ChatlistsCacheEntry {
    owner: Weak<LiveBridgeState>,
    state: LiveChatlistsCache,
}

fn chatlists_cache_registry() -> &'static Mutex<HashMap<usize, ChatlistsCacheEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, ChatlistsCacheEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_chatlists_cache<R>(live: &LiveSession, f: impl FnOnce(&mut LiveChatlistsCache) -> R) -> R
where
{
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    let mut map = chatlists_cache_registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    map.retain(|_, entry| entry.owner.strong_count() > 0);
    let entry = map.entry(key).or_insert_with(|| ChatlistsCacheEntry {
        owner: Arc::downgrade(live.bridge_state()),
        state: LiveChatlistsCache::default(),
    });
    f(&mut entry.state)
}

/// Invalidates cached suggested folders and chatlist invite metadata for `live`.
pub(crate) fn invalidate_cache(live: &LiveSession) {
    with_chatlists_cache(live, |c| {
        c.suggested_loaded = false;
        c.suggested.clear();
        c.links_loaded_filters.clear();
        c.links_by_filter.clear();
    });
}

// =============================================================================
// Internal RPC & TL conversion helpers
// =============================================================================

fn read_session<R>(live: &LiveSession, f: impl FnOnce(&Session) -> R + Send) -> R
where
    R: Send,
{
    let lock = live.runtime().session();
    if let Ok(guard) = lock.try_read() {
        return f(&guard);
    }
    let arc = Arc::clone(lock);
    block_on_runtime(live.runtime().tokio_handle(), async move {
        let guard = arc.read().await;
        f(&guard)
    })
}

fn write_session<R>(live: &LiveSession, f: impl FnOnce(&mut Session) -> R + Send) -> R
where
    R: Send,
{
    let lock = live.runtime().session();
    if let Ok(mut guard) = lock.try_write() {
        return f(&mut guard);
    }
    let arc = Arc::clone(lock);
    block_on_runtime(live.runtime().tokio_handle(), async move {
        let mut guard = arc.write().await;
        f(&mut guard)
    })
}

fn invoke_rpc<R>(live: &LiveSession, req: R) -> Result<R::Return, String>
where
    R: RemoteCall + Send + Sync + 'static,
    R::Return: Send + 'static,
{
    let rpc = live.runtime().rpc().clone();
    block_on_runtime(live.runtime().tokio_handle(), async move {
        with_rpc_timeout(CHATLISTS_RPC_TIMEOUT, async move { rpc.invoke(&req).await }).await
    })
}

fn tl_peer_to_ui_peer(peer: &tl_enums::Peer) -> PeerId {
    let data_peer = match peer {
        tl_enums::Peer::User(u) => DataPeerId::from_user(UserId(u.user_id as u64)),
        tl_enums::Peer::Chat(c) => DataPeerId::from_chat(ChatId(c.chat_id as u64)),
        tl_enums::Peer::Channel(c) => DataPeerId::from_channel(ChannelId(c.channel_id as u64)),
    };
    from_data_peer(data_peer)
}

fn tl_input_peer_to_data_peer(peer: &tl_enums::InputPeer) -> Option<DataPeerId> {
    match peer {
        tl_enums::InputPeer::User(u) => Some(DataPeerId::from_user(UserId(u.user_id as u64))),
        tl_enums::InputPeer::Chat(c) => Some(DataPeerId::from_chat(ChatId(c.chat_id as u64))),
        tl_enums::InputPeer::Channel(c) => {
            Some(DataPeerId::from_channel(ChannelId(c.channel_id as u64)))
        }
        _ => None,
    }
}

fn tl_dialog_filter_to_data(filter: &tl_enums::DialogFilter) -> Option<ChatFilter> {
    match filter {
        tl_enums::DialogFilter::DialogFilter(f) => {
            let mut flags = ChatFilterFlags::empty();
            if f.contacts {
                flags |= ChatFilterFlags::CONTACTS;
            }
            if f.non_contacts {
                flags |= ChatFilterFlags::NON_CONTACTS;
            }
            if f.groups {
                flags |= ChatFilterFlags::GROUPS;
            }
            if f.broadcasts {
                flags |= ChatFilterFlags::CHANNELS;
            }
            if f.bots {
                flags |= ChatFilterFlags::BOTS;
            }
            if f.exclude_muted {
                flags |= ChatFilterFlags::NO_MUTED;
            }
            if f.exclude_read {
                flags |= ChatFilterFlags::NO_READ;
            }
            if f.exclude_archived {
                flags |= ChatFilterFlags::NO_ARCHIVED;
            }
            let always: BTreeSet<DataPeerId> = f
                .include_peers
                .iter()
                .filter_map(tl_input_peer_to_data_peer)
                .collect();
            let pinned: Vec<DataPeerId> = f
                .pinned_peers
                .iter()
                .filter_map(tl_input_peer_to_data_peer)
                .collect();
            let never: BTreeSet<DataPeerId> = f
                .exclude_peers
                .iter()
                .filter_map(tl_input_peer_to_data_peer)
                .collect();
            let tl_enums::TextWithEntities::TextWithEntities(title_tl) = &f.title;
            Some(ChatFilter::new(
                f.id,
                DataText::plain(title_tl.text.clone()),
                f.emoticon.clone().unwrap_or_default(),
                f.color.and_then(|c| u8::try_from(c).ok()),
                flags,
                always,
                pinned,
                never,
            ))
        }
        tl_enums::DialogFilter::Chatlist(f) => {
            let mut flags = ChatFilterFlags::CHATLIST;
            if f.has_my_invites {
                flags |= ChatFilterFlags::HAS_MY_LINKS;
            }
            let always: BTreeSet<DataPeerId> = f
                .include_peers
                .iter()
                .filter_map(tl_input_peer_to_data_peer)
                .collect();
            let pinned: Vec<DataPeerId> = f
                .pinned_peers
                .iter()
                .filter_map(tl_input_peer_to_data_peer)
                .collect();
            let tl_enums::TextWithEntities::TextWithEntities(title_tl) = &f.title;
            Some(ChatFilter::new(
                f.id,
                DataText::plain(title_tl.text.clone()),
                f.emoticon.clone().unwrap_or_default(),
                f.color.and_then(|c| u8::try_from(c).ok()),
                flags,
                always,
                pinned,
                BTreeSet::new(),
            ))
        }
        tl_enums::DialogFilter::Default => None,
    }
}

fn resolve_input_peers(live: &LiveSession, peers: &[PeerId]) -> Vec<tl_enums::InputPeer> {
    read_session(live, |session| {
        peers
            .iter()
            .map(|&pid| input_peer_for(session, to_data_peer(pid)))
            .filter(|p| !matches!(p, tl_enums::InputPeer::Empty))
            .collect()
    })
}

fn apply_updates_users_chats(live: &LiveSession, updates: &tl_enums::Updates) {
    let _ = live.apply_updates_to_session(updates);
}

// =============================================================================
// SettingsBackend hooks for suggested folders
// =============================================================================

/// Returns suggested folders from `messages.getSuggestedDialogFilters`, caching per session.
pub(crate) fn suggested_folders(live: &LiveSession) -> Vec<SuggestedFolder> {
    let already_loaded = with_chatlists_cache(live, |c| c.suggested_loaded);
    if !already_loaded {
        let _ = live.fetch_suggested_folders();
    }
    let existing_titles: Vec<String> = read_session(live, |s| {
        s.filters()
            .list()
            .iter()
            .map(|f| f.title.text.to_ascii_lowercase())
            .collect()
    });
    with_chatlists_cache(live, |c| {
        c.suggested
            .iter()
            .filter(|item| !existing_titles.contains(&item.folder.title.to_ascii_lowercase()))
            .cloned()
            .collect()
    })
}

/// Accepts a suggested folder at `index`, persists it via `messages.updateDialogFilter`,
/// and removes it from the cached suggestion list.
pub(crate) fn accept_suggested_folder(
    live: &mut LiveSession,
    index: usize,
) -> Option<SettingsChatFolder> {
    let visible = suggested_folders(live);
    let chosen = visible.get(index)?.clone();
    let mut draft = chosen.folder.clone();
    draft.id = 0;
    let saved = live.save_folder(draft).ok()?;
    with_chatlists_cache(live, |c| {
        c.suggested
            .retain(|item| !item.folder.title.eq_ignore_ascii_case(&chosen.folder.title));
    });
    Some(saved)
}

// =============================================================================
// Public LiveSession API for suggested folders & shareable folder links
// =============================================================================

impl LiveSession {
    /// Fetches suggested chat folders directly via `messages.getSuggestedDialogFilters`
    /// and updates the per-session cache.
    pub fn fetch_suggested_folders(&self) -> Result<Vec<SuggestedFolder>, String> {
        let raw = invoke_rpc(self, tl_fns::messages::GetSuggestedDialogFilters {})?;
        let mut parsed = Vec::with_capacity(raw.len());
        for entry in raw {
            let tl_enums::DialogFilterSuggested::DialogFilterSuggested(s) = entry;
            if let Some(data_filter) = tl_dialog_filter_to_data(&s.filter) {
                parsed.push(SuggestedFolder {
                    folder: chat_filter_to_settings_folder(&data_filter),
                    description: s.description,
                });
            }
        }
        with_chatlists_cache(self, |c| {
            c.suggested_loaded = true;
            c.suggested = parsed.clone();
        });
        self.bridge_state().notify_ui();
        Ok(parsed)
    }

    /// Exports a shareable invite link for folder `filter_id` (`chatlists.exportChatlistInvite`).
    pub fn export_chatlist_invite(
        &self,
        filter_id: i32,
        title: &str,
        peers: &[PeerId],
    ) -> Result<ExportedChatlistLink, String> {
        let input_peers = resolve_input_peers(self, peers);
        let resp = invoke_rpc(
            self,
            tl_fns::chatlists::ExportChatlistInvite {
                chatlist: tl_enums::InputChatlist::DialogFilter(
                    tl_types::InputChatlistDialogFilter { filter_id },
                ),
                title: title.to_owned(),
                peers: input_peers,
            },
        )?;
        let tl_enums::chatlists::ExportedChatlistInvite::ExportedChatlistInvite(exported) = resp;
        if let Some(updated_filter) = tl_dialog_filter_to_data(&exported.filter) {
            write_session(self, |s| s.set_filter(updated_filter));
        }
        let tl_enums::ExportedChatlistInvite::ExportedChatlistInvite(inv) = exported.invite;
        let slug = extract_chatlist_slug(&inv.url);
        let link = ExportedChatlistLink {
            filter_id,
            title: inv.title,
            url: inv.url,
            slug,
            peers: inv.peers.iter().map(tl_peer_to_ui_peer).collect(),
        };
        with_chatlists_cache(self, |c| {
            let list = c.links_by_filter.entry(filter_id).or_default();
            list.retain(|existing| existing.url != link.url);
            list.push(link.clone());
        });
        self.bridge_state().notify_ui();
        Ok(link)
    }

    /// Inspects a shareable folder invite link or slug (`chatlists.checkChatlistInvite`).
    pub fn check_chatlist_invite(&self, slug_or_url: &str) -> Result<ChatlistInviteInfo, String> {
        let slug = extract_chatlist_slug(slug_or_url);
        if slug.is_empty() {
            return Err("Chatlist invite slug cannot be empty.".to_owned());
        }
        let resp = invoke_rpc(self, tl_fns::chatlists::CheckChatlistInvite { slug })?;
        let info = match resp {
            tl_enums::chatlists::ChatlistInvite::Already(already) => {
                write_session(self, |s| {
                    apply_tl_users_and_chats(s, &already.users, &already.chats);
                });
                ChatlistInviteInfo::Already {
                    filter_id: already.filter_id,
                    missing_peers: already
                        .missing_peers
                        .iter()
                        .map(tl_peer_to_ui_peer)
                        .collect(),
                    already_peers: already
                        .already_peers
                        .iter()
                        .map(tl_peer_to_ui_peer)
                        .collect(),
                }
            }
            tl_enums::chatlists::ChatlistInvite::ChatlistInvite(invite) => {
                write_session(self, |s| {
                    apply_tl_users_and_chats(s, &invite.users, &invite.chats);
                });
                let tl_enums::TextWithEntities::TextWithEntities(title_tl) = invite.title;
                ChatlistInviteInfo::New {
                    title: title_tl.text,
                    title_noanimate: invite.title_noanimate,
                    emoticon: invite.emoticon,
                    peers: invite.peers.iter().map(tl_peer_to_ui_peer).collect(),
                }
            }
        };
        self.bridge_state().notify_ui();
        Ok(info)
    }

    /// Joins a shareable folder invite link with the selected `peers` (`chatlists.joinChatlistInvite`).
    pub fn join_chatlist_invite(&self, slug_or_url: &str, peers: &[PeerId]) -> Result<(), String> {
        let slug = extract_chatlist_slug(slug_or_url);
        if slug.is_empty() {
            return Err("Chatlist invite slug cannot be empty.".to_owned());
        }
        let input_peers = resolve_input_peers(self, peers);
        let updates = invoke_rpc(
            self,
            tl_fns::chatlists::JoinChatlistInvite {
                slug,
                peers: input_peers,
            },
        )?;
        apply_updates_users_chats(self, &updates);
        self.invalidate_settings_caches();
        self.bridge_state().notify_ui();
        Ok(())
    }

    /// Checks for new chats added to a joined shareable folder (`chatlists.getChatlistUpdates`).
    pub fn get_chatlist_updates(&self, filter_id: i32) -> Result<ChatlistUpdatesInfo, String> {
        let resp = invoke_rpc(
            self,
            tl_fns::chatlists::GetChatlistUpdates {
                chatlist: tl_enums::InputChatlist::DialogFilter(
                    tl_types::InputChatlistDialogFilter { filter_id },
                ),
            },
        )?;
        let tl_enums::chatlists::ChatlistUpdates::ChatlistUpdates(upd) = resp;
        write_session(self, |s| {
            apply_tl_users_and_chats(s, &upd.users, &upd.chats);
        });
        let info = ChatlistUpdatesInfo {
            filter_id,
            missing_peers: upd.missing_peers.iter().map(tl_peer_to_ui_peer).collect(),
        };
        self.bridge_state().notify_ui();
        Ok(info)
    }

    /// Joins newly added peers returned by [`Self::get_chatlist_updates`] (`chatlists.joinChatlistUpdates`).
    pub fn join_chatlist_updates(&self, filter_id: i32, peers: &[PeerId]) -> Result<(), String> {
        let input_peers = resolve_input_peers(self, peers);
        let updates = invoke_rpc(
            self,
            tl_fns::chatlists::JoinChatlistUpdates {
                chatlist: tl_enums::InputChatlist::DialogFilter(
                    tl_types::InputChatlistDialogFilter { filter_id },
                ),
                peers: input_peers,
            },
        )?;
        apply_updates_users_chats(self, &updates);
        self.bridge_state().notify_ui();
        Ok(())
    }

    /// Dismisses pending chatlist update notifications (`chatlists.hideChatlistUpdates`).
    pub fn hide_chatlist_updates(&self, filter_id: i32) -> Result<bool, String> {
        let ok = invoke_rpc(
            self,
            tl_fns::chatlists::HideChatlistUpdates {
                chatlist: tl_enums::InputChatlist::DialogFilter(
                    tl_types::InputChatlistDialogFilter { filter_id },
                ),
            },
        )?;
        self.bridge_state().notify_ui();
        Ok(ok)
    }

    /// Fetches peers suggested to leave when removing a shared chat folder
    /// (`chatlists.getLeaveChatlistSuggestions`, `RemoveComplexChatFilter::request` in
    /// `api_chat_filters_remove_manager.cpp:98-108`).
    pub fn get_leave_chatlist_suggestions(&self, filter_id: i32) -> Result<Vec<PeerId>, String> {
        let peers = invoke_rpc(
            self,
            tl_fns::chatlists::GetLeaveChatlistSuggestions {
                chatlist: tl_enums::InputChatlist::DialogFilter(
                    tl_types::InputChatlistDialogFilter { filter_id },
                ),
            },
        )?;
        Ok(peers.iter().map(tl_peer_to_ui_peer).collect())
    }

    /// Leaves a shared folder and optionally leaves `peers_to_leave` (`chatlists.leaveChatlist`).
    pub fn leave_chatlist(&self, filter_id: i32, peers_to_leave: &[PeerId]) -> Result<(), String> {
        let input_peers = resolve_input_peers(self, peers_to_leave);
        let updates = invoke_rpc(
            self,
            tl_fns::chatlists::LeaveChatlist {
                chatlist: tl_enums::InputChatlist::DialogFilter(
                    tl_types::InputChatlistDialogFilter { filter_id },
                ),
                peers: input_peers,
            },
        )?;
        apply_updates_users_chats(self, &updates);
        write_session(self, |s| {
            let _ = s.remove_filter(filter_id);
        });
        with_chatlists_cache(self, |c| {
            c.links_by_filter.remove(&filter_id);
        });
        self.bridge_state().notify_ui();
        Ok(())
    }

    /// Fetches all exported shareable invite links for folder `filter_id`
    /// (`chatlists.getExportedInvites`) and updates the per-session cache.
    pub fn get_exported_chatlist_invites(
        &self,
        filter_id: i32,
    ) -> Result<Vec<ExportedChatlistLink>, String> {
        let resp = invoke_rpc(
            self,
            tl_fns::chatlists::GetExportedInvites {
                chatlist: tl_enums::InputChatlist::DialogFilter(
                    tl_types::InputChatlistDialogFilter { filter_id },
                ),
            },
        )?;
        let tl_enums::chatlists::ExportedInvites::ExportedInvites(data) = resp;
        write_session(self, |s| {
            apply_tl_users_and_chats(s, &data.users, &data.chats);
        });
        let links: Vec<ExportedChatlistLink> = data
            .invites
            .into_iter()
            .map(
                |tl_enums::ExportedChatlistInvite::ExportedChatlistInvite(inv)| {
                    let slug = extract_chatlist_slug(&inv.url);
                    ExportedChatlistLink {
                        filter_id,
                        title: inv.title,
                        url: inv.url,
                        slug,
                        peers: inv.peers.iter().map(tl_peer_to_ui_peer).collect(),
                    }
                },
            )
            .collect();
        with_chatlists_cache(self, |c| {
            c.links_loaded_filters.insert(filter_id, true);
            c.links_by_filter.insert(filter_id, links.clone());
        });
        self.bridge_state().notify_ui();
        Ok(links)
    }

    /// Edits the label and/or included peers of an exported shareable folder link
    /// (`chatlists.editExportedInvite`).
    pub fn edit_exported_chatlist_invite(
        &self,
        filter_id: i32,
        slug_or_url: &str,
        title: Option<&str>,
        peers: Option<&[PeerId]>,
    ) -> Result<ExportedChatlistLink, String> {
        let slug = extract_chatlist_slug(slug_or_url);
        if slug.is_empty() {
            return Err("Chatlist invite slug cannot be empty.".to_owned());
        }
        let clean_title = title.map(|t| {
            t.trim()
                .chars()
                .take(MAX_FOLDER_LINK_TITLE_LEN)
                .collect::<String>()
        });
        let input_peers = peers.map(|ps| resolve_input_peers(self, ps));
        let resp = invoke_rpc(
            self,
            tl_fns::chatlists::EditExportedInvite {
                chatlist: tl_enums::InputChatlist::DialogFilter(
                    tl_types::InputChatlistDialogFilter { filter_id },
                ),
                slug: slug.clone(),
                title: clean_title,
                peers: input_peers,
            },
        )?;
        let tl_enums::ExportedChatlistInvite::ExportedChatlistInvite(inv) = resp;
        let updated_slug = extract_chatlist_slug(&inv.url);
        let updated = ExportedChatlistLink {
            filter_id,
            title: inv.title,
            url: inv.url,
            slug: if updated_slug.is_empty() {
                slug.clone()
            } else {
                updated_slug
            },
            peers: inv.peers.iter().map(tl_peer_to_ui_peer).collect(),
        };
        with_chatlists_cache(self, |c| {
            let list = c.links_by_filter.entry(filter_id).or_default();
            if let Some(slot) = list
                .iter_mut()
                .find(|existing| existing.slug == slug || existing.url == updated.url)
            {
                *slot = updated.clone();
            } else {
                list.push(updated.clone());
            }
        });
        self.bridge_state().notify_ui();
        Ok(updated)
    }

    /// Revokes and deletes an exported shareable folder link (`chatlists.deleteExportedInvite`).
    pub fn delete_exported_chatlist_invite(
        &self,
        filter_id: i32,
        slug_or_url: &str,
    ) -> Result<bool, String> {
        let slug = extract_chatlist_slug(slug_or_url);
        if slug.is_empty() {
            return Err("Chatlist invite slug cannot be empty.".to_owned());
        }
        let ok = invoke_rpc(
            self,
            tl_fns::chatlists::DeleteExportedInvite {
                chatlist: tl_enums::InputChatlist::DialogFilter(
                    tl_types::InputChatlistDialogFilter { filter_id },
                ),
                slug: slug.clone(),
            },
        )?;
        if ok {
            with_chatlists_cache(self, |c| {
                if let Some(list) = c.links_by_filter.get_mut(&filter_id) {
                    list.retain(|existing| existing.slug != slug);
                }
            });
            self.bridge_state().notify_ui();
        }
        Ok(ok)
    }

    /// Returns whether chat folder color tags are enabled (`messages.dialogFilters.tags_enabled`).
    #[must_use]
    pub fn dialog_filter_tags_enabled(&self) -> bool {
        read_session(self, |s| s.filters().tags_enabled())
    }

    /// Enables or disables chat folder color tags (`messages.toggleDialogFilterTags`).
    pub fn toggle_dialog_filter_tags(&self, enabled: bool) -> Result<bool, String> {
        let ok = invoke_rpc(self, tl_fns::messages::ToggleDialogFilterTags { enabled })?;
        write_session(self, |s| s.set_filters_tags_enabled(enabled));
        self.bridge_state().notify_ui();
        Ok(ok)
    }

    /// Returns cached exported invite links for `filter_id`.
    #[must_use]
    pub fn cached_chatlist_links(&self, filter_id: i32) -> Vec<ExportedChatlistLink> {
        with_chatlists_cache(self, |c| {
            c.links_by_filter
                .get(&filter_id)
                .cloned()
                .unwrap_or_default()
        })
    }
}

// =============================================================================
// SettingsBackend hooks for folder multi-link manager & folder tags
// =============================================================================

fn exported_link_to_settings(link: &ExportedChatlistLink) -> FolderInviteLink {
    FolderInviteLink {
        filter_id: link.filter_id,
        title: link.title.clone(),
        url: link.url.clone(),
        slug: link.slug.clone(),
        peers: link.peers.iter().map(|p| to_data_peer(*p).0).collect(),
    }
}

fn raw_ids_to_ui_peers(ids: &[i64]) -> Vec<PeerId> {
    ids.iter()
        .copied()
        .map(|id| from_data_peer(DataPeerId(id)))
        .collect()
}

/// Settings hook for [`SettingsBackend::folder_invite_links`].
pub(crate) fn settings_folder_invite_links(
    live: &LiveSession,
    filter_id: i32,
) -> Vec<FolderInviteLink> {
    let already_loaded = with_chatlists_cache(live, |c| {
        c.links_loaded_filters
            .get(&filter_id)
            .copied()
            .unwrap_or(false)
    });
    if !already_loaded {
        let _ = live.get_exported_chatlist_invites(filter_id);
    }
    live.cached_chatlist_links(filter_id)
        .iter()
        .map(exported_link_to_settings)
        .collect()
}

/// Settings hook for [`SettingsBackend::create_folder_invite_link`].
pub(crate) fn settings_create_folder_invite_link(
    live: &LiveSession,
    filter_id: i32,
    title: &str,
    peers: &[i64],
) -> Result<FolderInviteLink, String> {
    let ui_peers = raw_ids_to_ui_peers(peers);
    let created = live.export_chatlist_invite(filter_id, title, &ui_peers)?;
    Ok(exported_link_to_settings(&created))
}

/// Settings hook for [`SettingsBackend::edit_folder_invite_link`].
pub(crate) fn settings_edit_folder_invite_link(
    live: &LiveSession,
    filter_id: i32,
    slug: &str,
    title: Option<&str>,
    peers: Option<&[i64]>,
) -> Result<FolderInviteLink, String> {
    let ui_peers = peers.map(raw_ids_to_ui_peers);
    let updated =
        live.edit_exported_chatlist_invite(filter_id, slug, title, ui_peers.as_deref())?;
    Ok(exported_link_to_settings(&updated))
}

/// Settings hook for [`SettingsBackend::delete_folder_invite_link`].
pub(crate) fn settings_delete_folder_invite_link(
    live: &LiveSession,
    filter_id: i32,
    slug: &str,
) -> Result<bool, String> {
    live.delete_exported_chatlist_invite(filter_id, slug)
}

/// Settings hook for [`SettingsBackend::check_folder_updates`].
pub(crate) fn settings_check_folder_updates(
    live: &LiveSession,
    filter_id: i32,
) -> Result<Vec<i64>, String> {
    let info = live.get_chatlist_updates(filter_id)?;
    Ok(info
        .missing_peers
        .into_iter()
        .map(|p| to_data_peer(p).0)
        .collect())
}

/// Settings hook for [`SettingsBackend::join_folder_updates`].
pub(crate) fn settings_join_folder_updates(
    live: &LiveSession,
    filter_id: i32,
    peers: &[i64],
) -> Result<(), String> {
    let ui_peers = raw_ids_to_ui_peers(peers);
    live.join_chatlist_updates(filter_id, &ui_peers)
}

/// Settings hook for [`SettingsBackend::leave_folder_suggestions`].
pub(crate) fn settings_leave_folder_suggestions(
    live: &LiveSession,
    filter_id: i32,
) -> Result<Vec<i64>, String> {
    let peers = live.get_leave_chatlist_suggestions(filter_id)?;
    Ok(peers.into_iter().map(|p| to_data_peer(p).0).collect())
}
