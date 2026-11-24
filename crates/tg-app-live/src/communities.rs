//! Live Layer-233 RPC backend for WP-27:
//! - Communities (`communities.create`, `togglePeerLink`, `getJoinedCommunities`,
//!   `toggleCommunityCollapsedInDialogs`, `getPeerLinkRequests`,
//!   `togglePeerLinkRequestApproval`, `toggleAllPeerLinkRequestApproval`,
//!   `toggleParticipantBanned`, `getParticipantJoinedChats`, plus `channels.getFullChannel`,
//!   `channels.editTitle`, `messages.editChatDefaultBannedRights`, `channels.deleteChannel`)
//! - Managed Bots (`bots.checkUsername`, `bots.createBot`, `bots.getBotInfo`,
//!   `bots.setBotInfo`, `bots.getAdminedBots`, `messages.translateText`)
//! - Group Moderation (`channels.deleteMessages`, `channels.reportSpam`,
//!   `channels.deleteParticipantHistory`, `channels.editBanned`,
//!   `communities.toggleParticipantBanned`)
//! - Welcome Messages (`ephemeral.getWelcomeMessages`, `ephemeral.sendMessage`,
//!   `ephemeral.editMessage`, `ephemeral.deleteWelcomeMessage`,
//!   `ephemeral.deleteAllWelcomeMessages`)
//! - Channel Anti-Spam (`channels.toggleAntiSpam`, `channels.reportAntiSpamFalsePositive`)
//!
//! Port origin:
//! - `Telegram/SourceFiles/api/api_communities.cpp`
//! - `Telegram/SourceFiles/api/api_bot_info.cpp`
//! - `Telegram/SourceFiles/boxes/peers/create_managed_bot_box.cpp`
//! - `Telegram/SourceFiles/boxes/peers/manage_community_box.cpp`
//! - `Telegram/SourceFiles/boxes/moderate_messages_box.cpp`
//! - `Telegram/SourceFiles/data/components/welcome_messages.cpp`
//! - `Telegram/SourceFiles/menu/menu_antispam_validator.cpp`

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tg_api::RpcClient;
use tg_api::convert::{apply_tl_users_and_chats, input_peer_for, input_user_for};
use tg_data::rights::{ChatAdminRights, ChatBannedRights, ChatRestrictionsInfo};
use tg_data::{ChannelFlags, ChannelId, ChatId, Peer, PeerId as DataPeerId, Session, UserId};
use tg_tl::{RemoteCall, enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::admin::banned_rights_to_tl;
use crate::bridge::LiveBridgeState;
use crate::convert::{from_data_peer, to_data_peer};
use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::session::LiveSession;
use tg_app_core::core_types::dto::community::{
    CommunityDetails, CommunityLinkedPeer, CommunityParticipantJoinedChats,
    CommunityPeerLinkAction, CommunityPeerRequest, CommunityPeerRequestsSlice,
    CreateCommunityRequest,
};
use tg_app_core::core_types::dto::managed_bot::{BotInfoChange, BotInfoTexts};
use tg_app_core::core_types::dto::moderate_messages::{
    ModerateMessagesRequest, ModerateMessagesResult,
};
use tg_app_core::core_types::dto::welcome_messages::WelcomeMessageItem;
use tg_app_core::core_types::misc::CommunityDialogGroup;
use tg_app_core::stand_in::types::PeerId;

/// Default RPC timeout for WP-27 requests.
const COMMUNITIES_RPC_TIMEOUT: Duration = Duration::from_secs(10);

static RANDOM_ID_COUNTER: AtomicI64 = AtomicI64::new(0x2700_0000_0001);

fn next_random_id() -> i64 {
    RANDOM_ID_COUNTER.fetch_add(1, Ordering::Relaxed)
}

fn now_unix_i32() -> i32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().min(i32::MAX as u64) as i32)
        .unwrap_or(0)
}

fn tl_peer_to_peer_id(peer: &tl_enums::Peer) -> DataPeerId {
    match peer {
        tl_enums::Peer::User(u) => DataPeerId::from_user(UserId::from_tl(u.user_id)),
        tl_enums::Peer::Chat(c) => DataPeerId::from_chat(ChatId::from_tl(c.chat_id)),
        tl_enums::Peer::Channel(ch) => DataPeerId::from_channel(ChannelId::from_tl(ch.channel_id)),
    }
}

#[derive(Default)]
struct CommunitiesLiveCache {
    details: HashMap<PeerId, CommunityDetails>,
    welcome_hashes: HashMap<PeerId, i64>,
    welcome_items: HashMap<PeerId, Vec<WelcomeMessageItem>>,
}

struct CommunitiesCacheEntry {
    owner: Weak<LiveBridgeState>,
    state: CommunitiesLiveCache,
}

fn cache_registry() -> &'static Mutex<HashMap<usize, CommunitiesCacheEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, CommunitiesCacheEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_cache<R>(live: &LiveSession, f: impl FnOnce(&mut CommunitiesLiveCache) -> R) -> R
where
{
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    let mut map = cache_registry().lock().unwrap_or_else(|e| e.into_inner());
    map.retain(|_, entry| entry.owner.strong_count() > 0);
    let entry = map.entry(key).or_insert_with(|| CommunitiesCacheEntry {
        owner: Arc::downgrade(live.bridge_state()),
        state: CommunitiesLiveCache::default(),
    });
    f(&mut entry.state)
}

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
        with_rpc_timeout(
            COMMUNITIES_RPC_TIMEOUT,
            async move { rpc.invoke(&req).await },
        )
        .await
    })
}

fn input_peer(live: &LiveSession, peer_id: PeerId) -> Option<tl_enums::InputPeer> {
    let ip = read_session(live, |s| input_peer_for(s, to_data_peer(peer_id)));
    (!matches!(ip, tl_enums::InputPeer::Empty)).then_some(ip)
}

fn input_channel(live: &LiveSession, peer_id: PeerId) -> Option<tl_enums::InputChannel> {
    let data_peer = to_data_peer(peer_id);
    read_session(live, |s| {
        let ch = s.peer(data_peer)?.as_channel()?;
        Some(tl_enums::InputChannel::InputChannel(
            tl_types::InputChannel {
                channel_id: ch.channel_id().0 as i64,
                access_hash: ch.access_hash as i64,
            },
        ))
    })
}

fn input_user(live: &LiveSession, peer_id: PeerId) -> Option<tl_enums::InputUser> {
    let data_peer = to_data_peer(peer_id);
    let uid = data_peer.to_user()?;
    let iu = read_session(live, |s| input_user_for(s, uid));
    if matches!(iu, tl_enums::InputUser::Empty) {
        Some(tl_enums::InputUser::InputUser(tl_types::InputUser {
            user_id: uid.to_tl(),
            access_hash: 0,
        }))
    } else {
        Some(iu)
    }
}

fn apply_updates_chats_users(live: &LiveSession, updates: &tl_enums::Updates) {
    let _ = live.apply_updates_to_session(updates);
}

fn extract_created_community_id(updates: &tl_enums::Updates) -> Option<PeerId> {
    let chats = match updates {
        tl_enums::Updates::Updates(u) => &u.chats,
        tl_enums::Updates::Combined(u) => &u.chats,
        _ => return None,
    };
    for chat in chats {
        if let tl_enums::Chat::Community(c) = chat {
            return Some(from_data_peer(DataPeerId::from_channel(
                ChannelId::from_tl(c.id),
            )));
        }
    }
    None
}

fn non_empty_opt(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_owned())
}

fn build_community_details_from_session(
    s: &Session,
    community_id: PeerId,
    full: Option<&tl_types::CommunityFull>,
) -> Option<CommunityDetails> {
    let data_pid = to_data_peer(community_id);
    let ch = s.peer(data_pid)?.as_channel()?;
    let collapsed = ch.flags.contains(ChannelFlags::COMMUNITY_COLLAPSED);
    let anyone_can_add = !ch
        .default_restrictions
        .contains(ChatBannedRights::MANAGE_LINKED_PEERS);
    let is_creator = ch.am_creator();
    let can_edit = is_creator || ch.admin_rights.contains(ChatAdminRights::CHANGE_INFO);
    let can_manage_linked_peers = is_creator
        || ch
            .admin_rights
            .contains(ChatAdminRights::MANAGE_LINKED_PEERS);

    let mut linked_peers = Vec::new();
    let (about, admins_count, kicked_count, pending_requests_count) = if let Some(f) = full {
        for lp in &f.linked_peers {
            let tl_enums::CommunityPeer::CommunityPeer(cp) = lp;
            let lp_data_id = tl_peer_to_peer_id(&cp.peer);
            let lp_peer_id = from_data_peer(lp_data_id);
            let (title, username, is_channel, is_bot, members_count, joined) =
                match s.peer(lp_data_id) {
                    Some(Peer::Channel(c)) => (
                        c.common.name.clone(),
                        non_empty_opt(c.usernames.username()),
                        c.is_broadcast(),
                        false,
                        c.members_count.max(0) as u32,
                        c.am_in(),
                    ),
                    Some(Peer::Chat(c)) => (
                        c.common.name.clone(),
                        None,
                        false,
                        false,
                        c.count.max(0) as u32,
                        !c.have_left(),
                    ),
                    Some(Peer::User(u)) => (
                        u.common.name.clone(),
                        non_empty_opt(u.usernames.username()),
                        false,
                        u.is_bot(),
                        0,
                        true,
                    ),
                    None => (
                        format!("Peer {}", lp_peer_id.bare()),
                        None,
                        false,
                        false,
                        0,
                        false,
                    ),
                };
            linked_peers.push(CommunityLinkedPeer {
                peer_id: lp_peer_id,
                title,
                username,
                is_channel,
                is_bot,
                members_count,
                joined,
                visible: cp.visible,
                can_view_history: cp.can_view_history,
                unread_count: 0,
                muted: false,
                last_message: String::new(),
                last_date_unix: 0,
                color_index: (lp_peer_id.bare() % 7) as u8,
            });
        }
        (
            f.about.clone(),
            f.admins_count.unwrap_or(0).max(0) as u32,
            f.kicked_count.unwrap_or(0).max(0) as u32,
            f.peer_link_requests_pending.unwrap_or(0).max(0) as u32,
        )
    } else {
        (
            ch.common.about.clone(),
            ch.admins_count.max(0) as u32,
            ch.kicked_count.max(0) as u32,
            0,
        )
    };

    Some(CommunityDetails {
        id: community_id,
        title: ch.common.name.clone(),
        about,
        collapsed_in_dialogs: collapsed,
        anyone_can_add_peers: anyone_can_add,
        can_edit,
        can_manage_linked_peers,
        can_delete: is_creator,
        admins_count,
        kicked_count,
        pending_requests_count,
        linked_peers,
        color_index: (community_id.bare() % 7) as u8,
    })
}

// =============================================================================
// 1. Communities (`api_communities.cpp` + `manage_community_box.cpp`)
// =============================================================================

/// Calls `communities.getJoinedCommunities` and returns all joined community hubs.
pub(crate) fn joined_communities(live: &LiveSession) -> Result<Vec<CommunityDetails>, String> {
    let resp = invoke_rpc(live, tl_fns::communities::GetJoinedCommunities {})?;
    let chats = match resp {
        tl_enums::messages::Chats::Chats(c) => c.chats,
        tl_enums::messages::Chats::Slice(c) => c.chats,
    };
    let mut ids = Vec::new();
    write_session(live, |s| {
        apply_tl_users_and_chats(s, &[], &chats);
        for chat in &chats {
            if let tl_enums::Chat::Community(c) = chat {
                ids.push(from_data_peer(DataPeerId::from_channel(
                    ChannelId::from_tl(c.id),
                )));
            }
        }
    });

    let mut out = Vec::with_capacity(ids.len());
    for cid in ids {
        let cached = with_cache(live, |c| c.details.get(&cid).cloned());
        if let Some(d) = cached {
            out.push(d);
        } else if let Some(d) =
            read_session(live, |s| build_community_details_from_session(s, cid, None))
        {
            with_cache(live, |c| {
                c.details.insert(cid, d.clone());
            });
            out.push(d);
        }
    }
    Ok(out)
}

/// Fetches full community metadata via `channels.getFullChannel` (handling `ChatFull::CommunityFull`).
pub(crate) fn community_details(
    live: &LiveSession,
    community_id: PeerId,
) -> Option<CommunityDetails> {
    if let Some(channel) = input_channel(live, community_id)
        && let Ok(tl_enums::messages::ChatFull::ChatFull(cf)) =
            invoke_rpc(live, tl_fns::channels::GetFullChannel { channel })
    {
        write_session(live, |s| {
            apply_tl_users_and_chats(s, &cf.users, &cf.chats);
        });
        if let tl_enums::ChatFull::CommunityFull(full) = &cf.full_chat
            && let Some(details) = read_session(live, |s| {
                build_community_details_from_session(s, community_id, Some(full))
            })
        {
            with_cache(live, |c| {
                c.details.insert(community_id, details.clone());
            });
            return Some(details);
        }
    }
    with_cache(live, |c| c.details.get(&community_id).cloned()).or_else(|| {
        read_session(live, |s| {
            build_community_details_from_session(s, community_id, None)
        })
    })
}

/// Finds the community linked to `peer_id`, if any.
pub(crate) fn community_for_peer(live: &LiveSession, peer_id: PeerId) -> Option<CommunityDetails> {
    let linked_cid = read_session(live, |s| {
        let dp = to_data_peer(peer_id);
        match s.peer(dp) {
            Some(Peer::Channel(ch)) if ch.is_community() => Some(peer_id),
            Some(Peer::Channel(ch)) => ch
                .linked_community_id
                .map(|id| from_data_peer(DataPeerId::from_channel(id))),
            Some(Peer::User(u)) => u
                .linked_community_id
                .map(|id| from_data_peer(DataPeerId::from_channel(id))),
            _ => None,
        }
    });
    if let Some(cid) = linked_cid {
        return community_details(live, cid);
    }
    with_cache(live, |c| {
        c.details
            .values()
            .find(|d| d.id == peer_id || d.linked_peers.iter().any(|lp| lp.peer_id == peer_id))
            .cloned()
    })
}

/// Builds [`CommunityDialogGroup`] descriptors from cached communities for dialogs grouping.
pub(crate) fn community_dialog_groups(live: &LiveSession) -> Vec<CommunityDialogGroup> {
    with_cache(live, |c| {
        c.details
            .values()
            .map(|d| CommunityDialogGroup {
                community_id: d.id,
                title: d.title.clone(),
                collapsed_in_dialogs: d.collapsed_in_dialogs,
                pinned: false,
                member_peer_ids: d
                    .linked_peers
                    .iter()
                    .filter(|p| p.joined)
                    .map(|p| p.peer_id)
                    .collect(),
                color_index: d.color_index,
            })
            .collect()
    })
}

/// Calls `communities.create` and extracts the created `Chat::Community` peer ID.
pub(crate) fn create_community(
    live: &LiveSession,
    req: CreateCommunityRequest,
) -> Result<PeerId, String> {
    let peer = input_peer(live, req.initial_peer_id)
        .ok_or_else(|| "Initial chat peer was not found.".to_owned())?;
    let about = (!req.about.trim().is_empty()).then(|| req.about.trim().to_owned());
    let updates = invoke_rpc(
        live,
        tl_fns::communities::Create {
            hidden: req.hidden,
            title: req.title.trim().to_owned(),
            about,
            peer,
        },
    )?;
    apply_updates_chats_users(live, &updates);
    let cid = extract_created_community_id(&updates)
        .ok_or_else(|| "Server response did not contain created community.".to_owned())?;
    let _ = community_details(live, cid);
    Ok(cid)
}

/// Calls `communities.togglePeerLink` (`visible`, `hidden`, or `deleted`).
pub(crate) fn toggle_community_peer_link(
    live: &LiveSession,
    community_id: PeerId,
    peer_id: PeerId,
    action: CommunityPeerLinkAction,
) -> Result<(), String> {
    let community =
        input_channel(live, community_id).ok_or_else(|| "Community not found.".to_owned())?;
    let peer = input_peer(live, peer_id).ok_or_else(|| "Target peer not found.".to_owned())?;
    let (visible, hidden, deleted) = match action {
        CommunityPeerLinkAction::Visible => (true, false, false),
        CommunityPeerLinkAction::Hidden => (false, true, false),
        CommunityPeerLinkAction::Deleted => (false, false, true),
    };
    invoke_rpc(
        live,
        tl_fns::communities::TogglePeerLink {
            visible,
            hidden,
            deleted,
            community,
            peer,
        },
    )?;
    with_cache(live, |c| {
        if let Some(d) = c.details.get_mut(&community_id) {
            match action {
                CommunityPeerLinkAction::Deleted => {
                    d.linked_peers.retain(|p| p.peer_id != peer_id);
                }
                CommunityPeerLinkAction::Visible | CommunityPeerLinkAction::Hidden => {
                    if let Some(lp) = d.linked_peers.iter_mut().find(|p| p.peer_id == peer_id) {
                        lp.visible = Some(visible);
                    }
                }
            }
        }
    });
    Ok(())
}

/// Calls `communities.toggleCommunityCollapsedInDialogs` with optimistic flag update + rollback
/// on RPC failure (exact port of `Communities::toggleCollapsedInDialogs`).
pub(crate) fn toggle_community_collapsed_in_dialogs(
    live: &LiveSession,
    community_id: PeerId,
    collapsed: bool,
) -> Result<(), String> {
    let community =
        input_channel(live, community_id).ok_or_else(|| "Community not found.".to_owned())?;
    let data_pid = to_data_peer(community_id);
    let prev = read_session(live, |s| {
        s.peer(data_pid)
            .and_then(|p| p.as_channel())
            .is_some_and(|ch| ch.flags.contains(ChannelFlags::COMMUNITY_COLLAPSED))
    });

    let apply_flag = |val: bool| {
        write_session(live, |s| {
            if let Some(Peer::Channel(ch)) = s.peer_mut(data_pid) {
                ch.flags.set(ChannelFlags::COMMUNITY_COLLAPSED, val);
            }
        });
        with_cache(live, |c| {
            if let Some(d) = c.details.get_mut(&community_id) {
                d.collapsed_in_dialogs = val;
            }
        });
    };

    apply_flag(collapsed);
    match invoke_rpc(
        live,
        tl_fns::communities::ToggleCommunityCollapsedInDialogs {
            collapsed,
            community,
        },
    ) {
        Ok(updates) => {
            apply_updates_chats_users(live, &updates);
            Ok(())
        }
        Err(err) => {
            apply_flag(prev);
            Err(err)
        }
    }
}

/// Calls `communities.getPeerLinkRequests`.
pub(crate) fn community_peer_link_requests(
    live: &LiveSession,
    community_id: PeerId,
    offset: &str,
    limit: i32,
) -> Result<CommunityPeerRequestsSlice, String> {
    let community =
        input_channel(live, community_id).ok_or_else(|| "Community not found.".to_owned())?;
    let tl_enums::communities::PeerLinkRequests::PeerLinkRequests(res) = invoke_rpc(
        live,
        tl_fns::communities::GetPeerLinkRequests {
            community,
            offset: offset.to_owned(),
            limit,
        },
    )?;

    write_session(live, |s| {
        apply_tl_users_and_chats(s, &res.users, &res.chats);
    });

    let list = read_session(live, |s| {
        res.requests
            .iter()
            .map(|r| {
                let tl_enums::CommunityPeerRequest::CommunityPeerRequest(fields) = r;
                let dp = tl_peer_to_peer_id(&fields.peer);
                let pid = from_data_peer(dp);
                let (peer_title, peer_username, is_channel, is_bot) = match s.peer(dp) {
                    Some(Peer::Channel(c)) => (
                        c.common.name.clone(),
                        non_empty_opt(c.usernames.username()),
                        c.is_broadcast(),
                        false,
                    ),
                    Some(Peer::Chat(c)) => (c.common.name.clone(), None, false, false),
                    Some(Peer::User(u)) => (
                        u.common.name.clone(),
                        non_empty_opt(u.usernames.username()),
                        false,
                        u.is_bot(),
                    ),
                    None => (format!("Peer {}", pid.bare()), None, false, false),
                };
                let req_uid = DataPeerId::from_user(UserId::from_tl(fields.requested_by));
                let requested_by_name = s
                    .peer(req_uid)
                    .and_then(|p| p.as_user())
                    .map(|u| u.common.name.clone())
                    .unwrap_or_default();
                CommunityPeerRequest {
                    peer_id: pid,
                    peer_title,
                    peer_username,
                    is_channel,
                    is_bot,
                    requested_by: Some(from_data_peer(req_uid)),
                    requested_by_name,
                    date: fields.date,
                    visible: fields.visible,
                    color_index: (pid.bare() % 7) as u8,
                }
            })
            .collect()
    });

    with_cache(live, |c| {
        if let Some(d) = c.details.get_mut(&community_id) {
            d.pending_requests_count = res.total_count.max(0) as u32;
        }
    });

    Ok(CommunityPeerRequestsSlice {
        list,
        next_offset: res.next_offset.unwrap_or_default(),
        total_count: res.total_count,
    })
}

/// Calls `communities.togglePeerLinkRequestApproval`.
pub(crate) fn toggle_community_peer_link_request_approval(
    live: &LiveSession,
    community_id: PeerId,
    peer_id: PeerId,
    reject: bool,
) -> Result<(), String> {
    let community =
        input_channel(live, community_id).ok_or_else(|| "Community not found.".to_owned())?;
    let peer = input_peer(live, peer_id).ok_or_else(|| "Target peer not found.".to_owned())?;
    invoke_rpc(
        live,
        tl_fns::communities::TogglePeerLinkRequestApproval {
            reject,
            community,
            peer,
        },
    )?;
    with_cache(live, |c| {
        if let Some(d) = c.details.get_mut(&community_id) {
            d.pending_requests_count = d.pending_requests_count.saturating_sub(1);
        }
    });
    Ok(())
}

/// Calls `communities.toggleAllPeerLinkRequestApproval`.
pub(crate) fn toggle_all_community_peer_link_request_approval(
    live: &LiveSession,
    community_id: PeerId,
    reject: bool,
) -> Result<(), String> {
    let community =
        input_channel(live, community_id).ok_or_else(|| "Community not found.".to_owned())?;
    invoke_rpc(
        live,
        tl_fns::communities::ToggleAllPeerLinkRequestApproval { reject, community },
    )?;
    with_cache(live, |c| {
        if let Some(d) = c.details.get_mut(&community_id) {
            d.pending_requests_count = 0;
        }
    });
    Ok(())
}

/// Calls `communities.toggleParticipantBanned`.
pub(crate) fn toggle_community_participant_banned(
    live: &LiveSession,
    community_id: PeerId,
    participant_id: PeerId,
    unban: bool,
) -> Result<(), String> {
    let community =
        input_channel(live, community_id).ok_or_else(|| "Community not found.".to_owned())?;
    let participant =
        input_peer(live, participant_id).ok_or_else(|| "Participant not found.".to_owned())?;
    invoke_rpc(
        live,
        tl_fns::communities::ToggleParticipantBanned {
            unban,
            community,
            participant,
        },
    )?;
    Ok(())
}

/// Calls `communities.getParticipantJoinedChats`.
pub(crate) fn community_participant_joined_chats(
    live: &LiveSession,
    community_id: PeerId,
    participant_id: PeerId,
) -> Result<CommunityParticipantJoinedChats, String> {
    let community =
        input_channel(live, community_id).ok_or_else(|| "Community not found.".to_owned())?;
    let participant =
        input_peer(live, participant_id).ok_or_else(|| "Participant not found.".to_owned())?;
    let tl_enums::communities::ParticipantJoinedChats::ParticipantJoinedChats(res) = invoke_rpc(
        live,
        tl_fns::communities::GetParticipantJoinedChats {
            community,
            participant,
        },
    )?;
    write_session(live, |s| {
        apply_tl_users_and_chats(s, &res.users, &res.chats);
    });
    Ok(CommunityParticipantJoinedChats {
        creator_chat_ids: res
            .creator_chat_ids
            .into_iter()
            .map(|id| from_data_peer(DataPeerId::from_channel(ChannelId::from_tl(id))))
            .collect(),
        joined_chat_ids: res
            .joined_chat_ids
            .into_iter()
            .map(|id| from_data_peer(DataPeerId::from_channel(ChannelId::from_tl(id))))
            .collect(),
    })
}

/// Updates community title (`channels.editTitle`) and who-can-add policy
/// (`messages.editChatDefaultBannedRights` with `MANAGE_LINKED_PEERS`).
pub(crate) fn update_community_settings(
    live: &LiveSession,
    community_id: PeerId,
    title: &str,
    only_admins_can_add: bool,
) -> Result<(), String> {
    let channel =
        input_channel(live, community_id).ok_or_else(|| "Community not found.".to_owned())?;
    let peer = input_peer(live, community_id).ok_or_else(|| "Community not found.".to_owned())?;

    let trimmed = title.trim().to_owned();
    let edit_res = invoke_rpc(
        live,
        tl_fns::channels::EditTitle {
            channel,
            title: trimmed.clone(),
        },
    );
    match edit_res {
        Ok(updates) => apply_updates_chats_users(live, &updates),
        Err(err) if err.contains("CHAT_NOT_MODIFIED") => {}
        Err(err) => return Err(err),
    }

    let rights = if only_admins_can_add {
        ChatBannedRights::MANAGE_LINKED_PEERS
    } else {
        ChatBannedRights::empty()
    };
    let rights_res = invoke_rpc(
        live,
        tl_fns::messages::EditChatDefaultBannedRights {
            peer,
            banned_rights: banned_rights_to_tl(rights, 0),
        },
    );
    match rights_res {
        Ok(updates) => apply_updates_chats_users(live, &updates),
        Err(err) if err.contains("CHAT_NOT_MODIFIED") => {}
        Err(err) => return Err(err),
    }

    let data_pid = to_data_peer(community_id);
    write_session(live, |s| {
        if let Some(Peer::Channel(ch)) = s.peer_mut(data_pid) {
            ch.common.name = trimmed.clone();
            ch.default_restrictions = rights;
        }
    });
    with_cache(live, |c| {
        if let Some(d) = c.details.get_mut(&community_id) {
            d.title = trimmed;
            d.anyone_can_add_peers = !only_admins_can_add;
        }
    });
    Ok(())
}

/// Deletes a community via `channels.deleteChannel`.
pub(crate) fn delete_community(live: &LiveSession, community_id: PeerId) -> Result<(), String> {
    let channel =
        input_channel(live, community_id).ok_or_else(|| "Community not found.".to_owned())?;
    match invoke_rpc(live, tl_fns::channels::DeleteChannel { channel }) {
        Ok(updates) => apply_updates_chats_users(live, &updates),
        Err(err) if err.contains("CHANNEL_NOT_MODIFIED") => {}
        Err(err) => return Err(err),
    }
    with_cache(live, |c| {
        c.details.remove(&community_id);
    });
    Ok(())
}

// =============================================================================
// 2. Managed Bots & Bot Info Localization (`create_managed_bot_box`, `api_bot_info`)
// =============================================================================

/// Calls `bots.checkUsername`.
pub(crate) fn check_bot_username(live: &LiveSession, username: &str) -> Result<bool, String> {
    invoke_rpc(
        live,
        tl_fns::bots::CheckUsername {
            additional: false,
            username: username.trim().to_owned(),
        },
    )
}

/// Calls `bots.createBot` and stores the returned `User` in `tg_data::Session`.
pub(crate) fn create_managed_bot(
    live: &LiveSession,
    manager_id: PeerId,
    name: &str,
    username: &str,
    via_deeplink: bool,
) -> Result<PeerId, String> {
    let manager_input =
        input_user(live, manager_id).ok_or_else(|| "Manager bot user not found.".to_owned())?;
    let user = invoke_rpc(
        live,
        tl_fns::bots::CreateBot {
            via_deeplink,
            name: name.trim().to_owned(),
            username: username.trim().to_owned(),
            manager_id: manager_input,
        },
    )?;
    let bot_id = match &user {
        tl_enums::User::User(u) => {
            Some(from_data_peer(DataPeerId::from_user(UserId::from_tl(u.id))))
        }
        tl_enums::User::Empty(e) => {
            Some(from_data_peer(DataPeerId::from_user(UserId::from_tl(e.id))))
        }
    };
    write_session(live, |s| {
        apply_tl_users_and_chats(s, &[user], &[]);
    });
    bot_id.ok_or_else(|| "Server did not return created bot user.".to_owned())
}

/// Calls `bots.getBotInfo`.
pub(crate) fn get_bot_info(
    live: &LiveSession,
    bot_id: PeerId,
    lang_code: &str,
) -> Result<BotInfoTexts, String> {
    let bot = input_user(live, bot_id);
    let tl_enums::bots::BotInfo::BotInfo(info) = invoke_rpc(
        live,
        tl_fns::bots::GetBotInfo {
            bot,
            lang_code: lang_code.to_owned(),
        },
    )?;
    Ok(BotInfoTexts {
        name: info.name,
        about: info.about,
        description: info.description,
    })
}

/// Calls `bots.setBotInfo`.
pub(crate) fn set_bot_info(
    live: &LiveSession,
    bot_id: PeerId,
    lang_code: &str,
    change: &BotInfoChange,
) -> Result<(), String> {
    let bot = input_user(live, bot_id);
    invoke_rpc(
        live,
        tl_fns::bots::SetBotInfo {
            bot,
            lang_code: lang_code.to_owned(),
            name: change.name.clone(),
            about: change.about.clone(),
            description: change.description.clone(),
        },
    )?;
    if lang_code.is_empty()
        && let Some(ref about) = change.about
    {
        let dp = to_data_peer(bot_id);
        write_session(live, |s| {
            if let Some(Peer::User(u)) = s.peer_mut(dp) {
                u.common.about = about.clone();
            }
        });
    }
    Ok(())
}

/// Translates bot info fields into `lang_code` via `messages.translateText` (`api_bot_info.cpp:65-92`).
pub(crate) fn translate_bot_info_texts(
    live: &LiveSession,
    texts: &[String],
    lang_code: &str,
) -> Result<Vec<String>, String> {
    let tl_texts: Vec<tl_enums::TextWithEntities> = texts
        .iter()
        .map(|t| {
            tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
                text: t.clone(),
                entities: Vec::new(),
            })
        })
        .collect();
    let tl_enums::messages::TranslatedText::TranslateResult(res) = invoke_rpc(
        live,
        tl_fns::messages::TranslateText {
            peer: None,
            id: None,
            text: Some(tl_texts),
            to_lang: lang_code.to_owned(),
            tone: None,
        },
    )?;
    Ok(res
        .result
        .into_iter()
        .map(|entry| {
            let tl_enums::TextWithEntities::TextWithEntities(t) = entry;
            t.text
        })
        .collect())
}

/// Calls `bots.getAdminedBots` and returns peer IDs of all bots managed/admined by this user.
pub(crate) fn admined_bots(live: &LiveSession) -> Result<Vec<PeerId>, String> {
    let users = invoke_rpc(live, tl_fns::bots::GetAdminedBots {})?;
    let mut ids = Vec::with_capacity(users.len());
    for u in &users {
        if let tl_enums::User::User(usr) = u {
            ids.push(from_data_peer(DataPeerId::from_user(UserId::from_tl(
                usr.id,
            ))));
        }
    }
    write_session(live, |s| {
        apply_tl_users_and_chats(s, &users, &[]);
    });
    Ok(ids)
}

// =============================================================================
// 3. Group Moderation (`ModerateMessagesBox`)
// =============================================================================

/// Executes `ModerateMessagesBox` actions against Telegram's group/channel RPCs:
/// `channels.deleteMessages`, `channels.reportSpam`, `channels.deleteParticipantHistory`,
/// `channels.editBanned`, and optionally `communities.toggleParticipantBanned`.
pub(crate) fn moderate_messages(
    live: &LiveSession,
    req: ModerateMessagesRequest,
) -> Result<ModerateMessagesResult, String> {
    let channel = input_channel(live, req.peer_id)
        .ok_or_else(|| "Channel/supergroup not found.".to_owned())?;
    let mut result = ModerateMessagesResult::default();

    // 1. Delete explicitly selected messages
    if !req.message_ids.is_empty() {
        let data_ids: Vec<tg_data::MsgId> = req
            .message_ids
            .iter()
            .map(|&id| tg_data::MsgId(i64::from(id)))
            .collect();
        live.delete_messages_via_runtime(req.peer_id, data_ids, true)?;
        result.deleted_selected = req.message_ids.len();
    }

    // 2. Report spam per participant
    for &pid in &req.report_spam_peers {
        if let Some(participant) = input_peer(live, pid) {
            invoke_rpc(
                live,
                tl_fns::channels::ReportSpam {
                    channel: channel.clone(),
                    participant,
                    id: req.message_ids.clone(),
                },
            )?;
            result.reported_peers += 1;
        }
    }

    // 3. Delete all messages from participant
    for &pid in &req.delete_all_from_peers {
        if let Some(participant) = input_peer(live, pid) {
            let tl_enums::messages::AffectedHistory::AffectedHistory(aff) = invoke_rpc(
                live,
                tl_fns::channels::DeleteParticipantHistory {
                    channel: channel.clone(),
                    participant,
                },
            )?;
            let _ = live.apply_affected_history(Some(req.peer_id), aff.pts, aff.pts_count);
            result.deleted_all_peers += 1;
        }
    }

    // 4. Ban or restrict participants
    // Match `ChannelData::KickedRestrictedRights` (`data/data_channel.cpp:495-514`):
    // when kicking (`restrict_duration_secs == None`), user peers receive `KICKED_USER`
    // while non-user peers (e.g. a channel posting as itself) receive `VIEW_MESSAGES` only.
    let restrict_until = req
        .restrict_duration_secs
        .map(|secs| now_unix_i32().saturating_add(secs));
    for &pid in &req.ban_peers {
        if let Some(participant) = input_peer(live, pid) {
            let restrictions = match restrict_until {
                None => tg_api::channels::kicked_restrictions_for_peer(to_data_peer(pid)),
                Some(until_date) => {
                    ChatRestrictionsInfo::new(ChatBannedRights::ALL_SEND, until_date)
                }
            };
            let edit_req =
                tg_api::channels::build_edit_banned(channel.clone(), participant, restrictions);
            let updates = invoke_rpc(live, edit_req)?;
            apply_updates_chats_users(live, &updates);
            result.banned_peers += 1;
        }
    }

    // 5. Optional community-wide ban
    if let Some(comm_id) = req.ban_in_community_id {
        for &pid in &req.ban_peers {
            toggle_community_participant_banned(live, comm_id, pid, false)?;
            result.community_banned_peers += 1;
        }
    }

    Ok(result)
}

// =============================================================================
// 4. Welcome Messages (`data/components/welcome_messages.cpp`)
// =============================================================================

fn tl_ephemeral_to_welcome_item(
    fallback_peer: PeerId,
    msg: &tl_enums::EphemeralMessage,
) -> WelcomeMessageItem {
    let tl_enums::EphemeralMessage::EphemeralMessage(m) = msg;
    let peer_id = m
        .peer_id
        .as_ref()
        .map(|p| from_data_peer(tl_peer_to_peer_id(p)))
        .unwrap_or(fallback_peer);
    let from_id = Some(from_data_peer(tl_peer_to_peer_id(&m.from_id)));
    WelcomeMessageItem {
        id: m.id,
        peer_id,
        from_id,
        date: m.date,
        text: m.message.clone(),
        out: m.out,
        welcome_template: m.welcome_template,
        invert_media: m.invert_media,
    }
}

/// Calls `ephemeral.getWelcomeMessages` with per-peer hash caching.
pub(crate) fn welcome_messages(
    live: &LiveSession,
    peer_id: PeerId,
) -> Result<Vec<WelcomeMessageItem>, String> {
    let peer = input_peer(live, peer_id).ok_or_else(|| "Peer not found.".to_owned())?;
    let hash = with_cache(live, |c| {
        c.welcome_hashes.get(&peer_id).copied().unwrap_or(0)
    });
    let resp = invoke_rpc(live, tl_fns::ephemeral::GetWelcomeMessages { peer, hash })?;
    match resp {
        tl_enums::ephemeral::WelcomeMessages::NotModified => Ok(with_cache(live, |c| {
            c.welcome_items.get(&peer_id).cloned().unwrap_or_default()
        })),
        tl_enums::ephemeral::WelcomeMessages::WelcomeMessages(data) => {
            let items: Vec<WelcomeMessageItem> = data
                .messages
                .iter()
                .map(|m| tl_ephemeral_to_welcome_item(peer_id, m))
                .collect();
            with_cache(live, |c| {
                c.welcome_hashes.insert(peer_id, data.hash);
                c.welcome_items.insert(peer_id, items.clone());
            });
            Ok(items)
        }
    }
}

/// Sends a new welcome message template via `ephemeral.sendMessage` (`welcome: true`).
pub(crate) fn send_welcome_message(
    live: &LiveSession,
    peer_id: PeerId,
    text: &str,
) -> Result<WelcomeMessageItem, String> {
    let peer = input_peer(live, peer_id).ok_or_else(|| "Peer not found.".to_owned())?;
    let trimmed = text.trim().to_owned();
    let updates = invoke_rpc(
        live,
        tl_fns::ephemeral::SendMessage {
            invert_media: false,
            welcome: true,
            anchor: false,
            noforwards: false,
            peer: Some(peer),
            receiver_id: tl_enums::InputUser::Empty,
            query_id: None,
            message: trimmed.clone(),
            entities: None,
            media: None,
            reply_markup: None,
            rich_message: None,
            random_id: next_random_id(),
            reply_to: None,
        },
    )?;
    apply_updates_chats_users(live, &updates);
    let now = now_unix_i32();
    let next_id = with_cache(live, |c| {
        let list = c.welcome_items.entry(peer_id).or_default();
        let id = list.iter().map(|m| m.id).max().unwrap_or(0) + 1;
        let item = WelcomeMessageItem {
            id,
            peer_id,
            from_id: None,
            date: now,
            text: trimmed.clone(),
            out: true,
            welcome_template: true,
            invert_media: false,
        };
        list.push(item.clone());
        c.welcome_hashes.insert(peer_id, 0);
        item
    });
    Ok(next_id)
}

/// Edits an existing welcome message template via `ephemeral.editMessage` (`welcome: true`).
pub(crate) fn edit_welcome_message(
    live: &LiveSession,
    peer_id: PeerId,
    id: i32,
    text: &str,
) -> Result<WelcomeMessageItem, String> {
    let peer = input_peer(live, peer_id).ok_or_else(|| "Peer not found.".to_owned())?;
    let trimmed = text.trim().to_owned();
    let updates = invoke_rpc(
        live,
        tl_fns::ephemeral::EditMessage {
            invert_media: false,
            welcome: true,
            peer: Some(peer),
            receiver_id: tl_enums::InputUser::Empty,
            id,
            message: Some(trimmed.clone()),
            media: None,
            entities: None,
            reply_markup: None,
            rich_message: None,
        },
    )?;
    apply_updates_chats_users(live, &updates);
    let now = now_unix_i32();
    let updated = with_cache(live, |c| {
        let list = c.welcome_items.entry(peer_id).or_default();
        if let Some(existing) = list.iter_mut().find(|m| m.id == id) {
            existing.text = trimmed.clone();
            existing.clone()
        } else {
            let item = WelcomeMessageItem {
                id,
                peer_id,
                from_id: None,
                date: now,
                text: trimmed.clone(),
                out: true,
                welcome_template: true,
                invert_media: false,
            };
            list.push(item.clone());
            item
        }
    });
    Ok(updated)
}

/// Deletes a single welcome message template via `ephemeral.deleteWelcomeMessage`.
pub(crate) fn delete_welcome_message(
    live: &LiveSession,
    peer_id: PeerId,
    id: i32,
) -> Result<(), String> {
    let peer = input_peer(live, peer_id).ok_or_else(|| "Peer not found.".to_owned())?;
    invoke_rpc(live, tl_fns::ephemeral::DeleteWelcomeMessage { peer, id })?;
    with_cache(live, |c| {
        if let Some(list) = c.welcome_items.get_mut(&peer_id) {
            list.retain(|m| m.id != id);
        }
    });
    Ok(())
}

/// Deletes all welcome message templates via `ephemeral.deleteAllWelcomeMessages`.
pub(crate) fn delete_all_welcome_messages(
    live: &LiveSession,
    peer_id: PeerId,
) -> Result<(), String> {
    let peer = input_peer(live, peer_id).ok_or_else(|| "Peer not found.".to_owned())?;
    invoke_rpc(live, tl_fns::ephemeral::DeleteAllWelcomeMessages { peer })?;
    with_cache(live, |c| {
        c.welcome_items.remove(&peer_id);
        c.welcome_hashes.insert(peer_id, 0);
    });
    Ok(())
}

// =============================================================================
// 5. Channel Anti-Spam (`channels.toggleAntiSpam`, `reportAntiSpamFalsePositive`)
// =============================================================================

/// Reads `ChannelFlags::ANTI_SPAM` for `peer_id` from `tg_data::Session`.
pub(crate) fn channel_anti_spam_enabled(live: &LiveSession, peer_id: PeerId) -> bool {
    let dp = to_data_peer(peer_id);
    read_session(live, |s| {
        s.peer(dp)
            .and_then(|p| p.as_channel())
            .is_some_and(|ch| ch.flags.contains(ChannelFlags::ANTI_SPAM))
    })
}

/// Toggles aggressive anti-spam on a supergroup/channel via `channels.toggleAntiSpam`
/// and updates `ChannelFlags::ANTI_SPAM` in `tg_data::Session`.
pub(crate) fn toggle_channel_anti_spam(
    live: &LiveSession,
    peer_id: PeerId,
    enabled: bool,
) -> Result<(), String> {
    let channel =
        input_channel(live, peer_id).ok_or_else(|| "Channel/supergroup not found.".to_owned())?;
    let updates = invoke_rpc(live, tl_fns::channels::ToggleAntiSpam { channel, enabled })?;
    apply_updates_chats_users(live, &updates);
    let dp = to_data_peer(peer_id);
    write_session(live, |s| {
        if let Some(Peer::Channel(ch)) = s.peer_mut(dp) {
            ch.flags.set(ChannelFlags::ANTI_SPAM, enabled);
        }
    });
    Ok(())
}

/// Reports a false-positive anti-spam deletion via `channels.reportAntiSpamFalsePositive`.
pub(crate) fn report_antispam_false_positive(
    live: &LiveSession,
    peer_id: PeerId,
    msg_id: i32,
) -> Result<(), String> {
    let channel =
        input_channel(live, peer_id).ok_or_else(|| "Channel/supergroup not found.".to_owned())?;
    invoke_rpc(
        live,
        tl_fns::channels::ReportAntiSpamFalsePositive { channel, msg_id },
    )?;
    Ok(())
}
