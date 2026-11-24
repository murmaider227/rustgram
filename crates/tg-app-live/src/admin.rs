//! Live admin/group mutations (`InfoBackend`), 2FA cloud password management (`SettingsBackend`),
//! and Telegram data export kickoff (`tg-export`'s [`TlExportSource`]) for [`LiveSession`].
//!
//! Port origin:
//! - `Telegram/SourceFiles/boxes/peers/edit_peer_info_box.cpp`
//! - `Telegram/SourceFiles/boxes/peers/edit_peer_permissions_box.cpp`
//! - `Telegram/SourceFiles/boxes/peers/edit_participant_box.cpp`
//! - `Telegram/SourceFiles/api/api_chat_participants.cpp`
//! - `Telegram/SourceFiles/api/api_cloud_password.cpp`
//! - `Telegram/SourceFiles/export/export_manager.cpp`

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use tg_api::RpcClient;
use tg_api::account::{CloudPasswordManager, ResetPasswordOutcome};
use tg_api::convert::{apply_tl_users_and_chats, input_peer_for, input_user_for};
use tg_crypto::srp::compute_check;
use tg_data::rights::{ChatAdminRights, ChatBannedRights, ChatRestrictionsInfo};
use tg_data::{
    ChannelFlags, ChannelId, ChatFlags, DocumentId, Peer, PeerId as DataPeerId, Session, UserId,
};
use tg_export::{
    Environment, ExportControlHandle, ExportController, ExportRpc, ExportTypes, MediaSettings,
    MediaTypes, OutputFormat, RpcCallError, Settings as ExportSettings, TlExportSource,
};
use tg_passport::controller::{parse_password_kdf_algo, srp_check_to_tl};
use tg_tl::{Identifiable, RemoteCall, enums as tl_enums, functions as tl_fns, types as tl_types};
use tracing::warn;

use crate::bridge::LiveBridgeState;
use crate::convert::{from_data_peer, to_data_peer};
use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::session::LiveSession;
use tg_app_core::info::backend::{
    CreateChannelRequest, CreateGroupRequest, DownloadEntry, DownloadStatus, InviteImporterInfo,
    InviteImportersList, InviteLinkInfo, InviteLinkRequest, InviteLinksList, JoinRequestsSummary,
    OwnershipTransferCheck, OwnershipTransferError, PeerColorSettings, SimilarChannelEntry,
    SimilarChannelsResult, UpdatePeerColorRequest, UpdatePeerInfoRequest,
};
use tg_app_core::settings::backend::{
    CloudPasswordError, CloudPasswordResetOutcome, CloudPasswordState, ExportFormat, ExportRequest,
    SettingsBackend,
};
use tg_app_core::stand_in::types::PeerId;

/// Default timeout for synchronous admin and 2FA RPC invocations.
const ADMIN_RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// Per-session state tracking active data export handles and live Downloads Manager entries.
#[derive(Default)]
struct AdminLiveState {
    export_handle: Option<ExportControlHandle>,
    downloads: Vec<DownloadEntry>,
    next_download_id: u64,
}

struct AdminStateEntry {
    owner: Weak<LiveBridgeState>,
    state: AdminLiveState,
}

fn admin_state_registry() -> &'static Mutex<HashMap<usize, AdminStateEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, AdminStateEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_admin_state<R>(live: &LiveSession, f: impl FnOnce(&mut AdminLiveState) -> R) -> R
where
{
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    let mut map = admin_state_registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    map.retain(|_, entry| entry.owner.strong_count() > 0);
    let entry = map.entry(key).or_insert_with(|| AdminStateEntry {
        owner: Arc::downgrade(live.bridge_state()),
        state: AdminLiveState::default(),
    });
    f(&mut entry.state)
}

// -----------------------------------------------------------------------------
// Internal RPC & Session helpers
// -----------------------------------------------------------------------------

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
        with_rpc_timeout(ADMIN_RPC_TIMEOUT, async move { rpc.invoke(&req).await }).await
    })
}

fn invoke_rpc_raw<R>(live: &LiveSession, req: R) -> Result<R::Return, tg_api::rpc::InvocationError>
where
    R: RemoteCall + Send + Sync + 'static,
    R::Return: Send + 'static,
{
    let rpc = live.runtime().rpc().clone();
    block_on_runtime(live.runtime().tokio_handle(), async move {
        match tokio::time::timeout(ADMIN_RPC_TIMEOUT, rpc.invoke(&req)).await {
            Ok(Ok(val)) => Ok(val),
            Ok(Err(err)) => Err(err),
            Err(_) => Err(tg_api::rpc::InvocationError::Dropped),
        }
    })
}

fn apply_updates_users_chats(live: &LiveSession, updates: &tl_enums::Updates) -> Vec<DataPeerId> {
    let mut created_peers = Vec::new();
    match updates {
        tl_enums::Updates::Updates(u) => {
            for chat in &u.chats {
                if let Some(pid) = tl_chat_peer_id(chat) {
                    created_peers.push(pid);
                }
            }
        }
        tl_enums::Updates::Combined(u) => {
            for chat in &u.chats {
                if let Some(pid) = tl_chat_peer_id(chat) {
                    created_peers.push(pid);
                }
            }
        }
        _ => {}
    }
    let _ = live.apply_updates_to_session(updates);
    created_peers
}

fn tl_chat_peer_id(chat: &tl_enums::Chat) -> Option<DataPeerId> {
    match chat {
        tl_enums::Chat::Chat(c) => Some(DataPeerId::from_chat(tg_data::ChatId::from_tl(c.id))),
        tl_enums::Chat::Channel(ch) => Some(DataPeerId::from_channel(ChannelId::from_tl(ch.id))),
        tl_enums::Chat::Forbidden(c) => Some(DataPeerId::from_chat(tg_data::ChatId::from_tl(c.id))),
        tl_enums::Chat::ChannelForbidden(ch) => {
            Some(DataPeerId::from_channel(ChannelId::from_tl(ch.id)))
        }
        _ => None,
    }
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

// -----------------------------------------------------------------------------
// Rights conversions (ChatBannedRights -> TL)
// -----------------------------------------------------------------------------

/// Converts [`ChatBannedRights`] and `until_date` into TL `ChatBannedRights` via
/// [`tg_api::convert::banned_rights_to_tl`] (`RestrictionsToMTP` in
/// `data/data_chat_participant_status.cpp:147-177`), never setting legacy
/// `send_messages` / `send_media` bits and normalizing forever `until_date` to `0`.
#[must_use]
pub(crate) fn banned_rights_to_tl(
    r: ChatBannedRights,
    until_date: i32,
) -> tl_enums::ChatBannedRights {
    tg_api::convert::banned_rights_to_tl(ChatRestrictionsInfo::new(
        r,
        tg_api::channels::normalize_ban_until_date(until_date),
    ))
}

// -----------------------------------------------------------------------------
// InfoBackend admin & group mutation helpers
// -----------------------------------------------------------------------------

/// Creates a new basic group via `messages.createChat`.
pub(crate) fn create_group(
    live: &mut LiveSession,
    req: CreateGroupRequest,
) -> Result<PeerId, String> {
    let title = req.title.trim();
    if title.is_empty() {
        return Err("Group title cannot be empty.".to_owned());
    }
    let users: Vec<tl_enums::InputUser> = req
        .member_ids
        .iter()
        .filter_map(|&pid| input_user(live, pid))
        .collect();
    let ttl_period =
        (req.auto_delete_ttl_seconds > 0).then_some(req.auto_delete_ttl_seconds as i32);
    let tl_enums::messages::InvitedUsers::InvitedUsers(invited) = invoke_rpc(
        live,
        tl_fns::messages::CreateChat {
            users,
            title: title.to_owned(),
            ttl_period,
        },
    )?;
    let created = apply_updates_users_chats(live, &invited.updates);
    let new_peer = created
        .into_iter()
        .find(|p| p.is_chat() || p.is_channel())
        .ok_or_else(|| "Server did not return created group in updates.".to_owned())?;
    live.bridge_state().notify_ui();
    Ok(from_data_peer(new_peer))
}

/// Creates a new broadcast channel via `channels.createChannel`, applying optional
/// public username (`channels.updateUsername`) and content protection (`messages.toggleNoForwards`).
pub(crate) fn create_channel(
    live: &mut LiveSession,
    req: CreateChannelRequest,
) -> Result<PeerId, String> {
    let title = req.title.trim();
    if title.is_empty() {
        return Err("Channel title cannot be empty.".to_owned());
    }
    let updates = invoke_rpc(
        live,
        tl_fns::channels::CreateChannel {
            broadcast: true,
            megagroup: false,
            for_import: false,
            forum: false,
            title: title.to_owned(),
            about: req.description.trim().to_owned(),
            geo_point: None,
            address: None,
            ttl_period: None,
        },
    )?;
    let created = apply_updates_users_chats(live, &updates);
    let new_peer_data = created
        .into_iter()
        .find(|p| p.is_channel())
        .ok_or_else(|| "Server did not return created channel in updates.".to_owned())?;
    let new_peer = from_data_peer(new_peer_data);

    if req.is_public
        && let Some(ref uname) = req.username
    {
        let cleaned = uname.trim().trim_start_matches('@');
        if !cleaned.is_empty() {
            update_username(live, new_peer, cleaned)?;
        }
    }
    if req.no_forwards
        && let Some(ip) = input_peer(live, new_peer)
    {
        let upd = invoke_rpc(
            live,
            tl_fns::messages::ToggleNoForwards {
                peer: ip,
                enabled: true,
                request_msg_id: None,
            },
        )?;
        let _ = apply_updates_users_chats(live, &upd);
    }

    live.bridge_state().notify_ui();
    Ok(new_peer)
}

/// Edits a group or channel's title, description, username, and no-forwards setting.
pub(crate) fn update_peer_info(
    live: &mut LiveSession,
    peer_id: PeerId,
    req: UpdatePeerInfoRequest,
) -> Result<(), String> {
    let data_peer = to_data_peer(peer_id);
    let is_self = read_session(live, |s| s.self_peer_id() == data_peer);
    if is_self {
        let mut parts = req.title.trim().splitn(2, ' ');
        let first = parts.next().unwrap_or_default();
        let last = parts.next().unwrap_or_default();
        SettingsBackend::update_name(live, first, last)?;
        SettingsBackend::update_bio(live, &req.description)?;
        if let Some(ref uname) = req.username {
            SettingsBackend::update_username(live, uname)?;
        }
        return Ok(());
    }

    let title = req.title.trim();
    if title.is_empty() {
        return Err("Chat title cannot be empty.".to_owned());
    }
    let about = req.description.trim();

    enum PeerKindSnapshot {
        Chat {
            chat_id: i64,
            old_title: String,
            old_about: String,
            old_no_forwards: bool,
        },
        Channel {
            input: tl_enums::InputChannel,
            old_title: String,
            old_about: String,
            old_username: String,
            old_no_forwards: bool,
        },
    }

    let snapshot = read_session(live, |s| match s.peer(data_peer)? {
        Peer::Chat(chat) => Some(PeerKindSnapshot::Chat {
            chat_id: chat.chat_id().0 as i64,
            old_title: chat.common.name.clone(),
            old_about: chat.common.about.clone(),
            old_no_forwards: chat.flags.contains(ChatFlags::NO_FORWARDS),
        }),
        Peer::Channel(ch) => Some(PeerKindSnapshot::Channel {
            input: tl_enums::InputChannel::InputChannel(tl_types::InputChannel {
                channel_id: ch.channel_id().0 as i64,
                access_hash: ch.access_hash as i64,
            }),
            old_title: ch.common.name.clone(),
            old_about: ch.common.about.clone(),
            old_username: ch.usernames.editable_username().to_owned(),
            old_no_forwards: ch.flags.contains(ChannelFlags::NO_FORWARDS),
        }),
        Peer::User(_) => None,
    })
    .ok_or_else(|| "Peer not found or not editable.".to_owned())?;

    let ip = input_peer(live, peer_id).ok_or_else(|| "Peer not found in session.".to_owned())?;

    match snapshot {
        PeerKindSnapshot::Chat {
            chat_id,
            old_title,
            old_about,
            old_no_forwards,
        } => {
            if title != old_title {
                let upd = invoke_rpc(
                    live,
                    tl_fns::messages::EditChatTitle {
                        chat_id,
                        title: title.to_owned(),
                    },
                )?;
                let _ = apply_updates_users_chats(live, &upd);
            }
            if about != old_about {
                let _ = invoke_rpc(
                    live,
                    tl_fns::messages::EditChatAbout {
                        peer: ip.clone(),
                        about: about.to_owned(),
                    },
                )?;
            }
            if req.no_forwards != old_no_forwards {
                let upd = invoke_rpc(
                    live,
                    tl_fns::messages::ToggleNoForwards {
                        peer: ip,
                        enabled: req.no_forwards,
                        request_msg_id: None,
                    },
                )?;
                let _ = apply_updates_users_chats(live, &upd);
            }
            write_session(live, |s| {
                if let Some(Peer::Chat(chat)) = s.peer_mut(data_peer) {
                    chat.common.name = title.to_owned();
                    chat.common.about = about.to_owned();
                    chat.flags.set(ChatFlags::NO_FORWARDS, req.no_forwards);
                }
            });
        }
        PeerKindSnapshot::Channel {
            input,
            old_title,
            old_about,
            old_username,
            old_no_forwards,
        } => {
            if title != old_title {
                let upd = invoke_rpc(
                    live,
                    tl_fns::channels::EditTitle {
                        channel: input.clone(),
                        title: title.to_owned(),
                    },
                )?;
                let _ = apply_updates_users_chats(live, &upd);
            }
            if about != old_about {
                let _ = invoke_rpc(
                    live,
                    tl_fns::messages::EditChatAbout {
                        peer: ip.clone(),
                        about: about.to_owned(),
                    },
                )?;
            }
            let desired_uname = if req.is_public {
                req.username
                    .as_deref()
                    .unwrap_or(&old_username)
                    .trim()
                    .trim_start_matches('@')
                    .to_owned()
            } else {
                String::new()
            };
            if desired_uname != old_username {
                let _ = invoke_rpc(
                    live,
                    tl_fns::channels::UpdateUsername {
                        channel: input,
                        username: desired_uname.clone(),
                    },
                )?;
            }
            if req.no_forwards != old_no_forwards {
                let upd = invoke_rpc(
                    live,
                    tl_fns::messages::ToggleNoForwards {
                        peer: ip,
                        enabled: req.no_forwards,
                        request_msg_id: None,
                    },
                )?;
                let _ = apply_updates_users_chats(live, &upd);
            }
            write_session(live, |s| {
                if let Some(Peer::Channel(ch)) = s.peer_mut(data_peer) {
                    ch.set_name(title, desired_uname);
                    ch.common.about = about.to_owned();
                    ch.flags.set(ChannelFlags::NO_FORWARDS, req.no_forwards);
                }
            });
        }
    }

    live.bridge_state().notify_ui();
    Ok(())
}

/// Updates default banned rights (`messages.editChatDefaultBannedRights`) and slow-mode delay
/// (`channels.toggleSlowMode`).
pub(crate) fn update_default_permissions(
    live: &mut LiveSession,
    peer_id: PeerId,
    rights: ChatBannedRights,
    slowmode_seconds: u32,
) -> Result<(), String> {
    let ip = input_peer(live, peer_id).ok_or_else(|| "Peer not found in session.".to_owned())?;
    let upd = invoke_rpc(
        live,
        tl_fns::messages::EditChatDefaultBannedRights {
            peer: ip,
            banned_rights: banned_rights_to_tl(rights, 0),
        },
    )?;
    let _ = apply_updates_users_chats(live, &upd);

    let data_peer = to_data_peer(peer_id);
    if let Some(ch) = input_channel(live, peer_id) {
        let prev_slowmode = read_session(live, |s| {
            s.peer(data_peer)
                .and_then(|p| p.as_channel())
                .map_or(0, |c| c.slowmode_seconds())
        });
        let target_slowmode = slowmode_seconds as i32;
        if target_slowmode != prev_slowmode {
            let upd = invoke_rpc(
                live,
                tl_fns::channels::ToggleSlowMode {
                    channel: ch,
                    seconds: target_slowmode,
                },
            )?;
            let _ = apply_updates_users_chats(live, &upd);
        }
    }

    write_session(live, |s| match s.peer_mut(data_peer) {
        Some(Peer::Channel(ch)) => {
            ch.default_restrictions = rights;
            ch.set_slowmode_seconds(slowmode_seconds as i32);
        }
        Some(Peer::Chat(chat)) => {
            chat.default_restrictions = rights;
        }
        _ => {}
    });
    live.bridge_state().notify_ui();
    Ok(())
}

/// Promotes, updates, or demotes an administrator via `channels.editAdmin` (or
/// `messages.editChatAdmin` for basic groups).
///
/// Matches `RemoveAdmin` and `SaveChannelAdmin` in
/// `boxes/peers/edit_participants_box.cpp:63-78, 165-183`:
/// in Layer 233 `rank:flags.0?string` distinguishes `None` (leave unchanged) from `Some("")`
/// (clear custom title), so both clearing the rank and demoting (`rights.is_empty()`) must send
/// `rank: Some("")`.
pub(crate) fn update_admin_rights(
    live: &mut LiveSession,
    peer_id: PeerId,
    user_id: PeerId,
    rights: ChatAdminRights,
    custom_title: String,
) -> Result<(), String> {
    let target_user = input_user(live, user_id)
        .ok_or_else(|| "Administrator target must be a user.".to_owned())?;
    if let Some(channel) = input_channel(live, peer_id) {
        // Match `RemoveAdmin` / `SaveChannelAdmin` (`boxes/peers/edit_participants_box.cpp:63-78,
        // 165-183`): explicitly send `rank: Some("")` when clearing the rank or demoting.
        let effective_rank = if rights.is_empty() {
            ""
        } else {
            custom_title.trim()
        };
        let req =
            tg_api::channels::build_edit_admin(channel, target_user, rights, Some(effective_rank));
        let upd = invoke_rpc(live, req)?;
        let _ = apply_updates_users_chats(live, &upd);
    } else if let Some(chat_id) = to_data_peer(peer_id).to_chat() {
        let _ = invoke_rpc(
            live,
            tl_fns::messages::EditChatAdmin {
                chat_id: chat_id.0 as i64,
                user_id: target_user,
                is_admin: !rights.is_empty(),
            },
        )?;
    } else {
        return Err("Peer is not a group or channel.".to_owned());
    }
    live.bridge_state().notify_ui();
    Ok(())
}

/// Invites members via `channels.inviteToChannel` (supergroups/channels) or
/// `messages.addChatUser` (basic groups).
pub(crate) fn add_members(
    live: &mut LiveSession,
    peer_id: PeerId,
    member_ids: &[PeerId],
) -> Result<usize, String> {
    if member_ids.is_empty() {
        return Ok(0);
    }
    if let Some(channel) = input_channel(live, peer_id) {
        let users: Vec<tl_enums::InputUser> = member_ids
            .iter()
            .filter_map(|&pid| input_user(live, pid))
            .collect();
        let total = users.len();
        let tl_enums::messages::InvitedUsers::InvitedUsers(resp) =
            invoke_rpc(live, tl_fns::channels::InviteToChannel { channel, users })?;
        let _ = apply_updates_users_chats(live, &resp.updates);
        live.bridge_state().notify_ui();
        return Ok(total.saturating_sub(resp.missing_invitees.len()));
    }
    if let Some(chat_id) = to_data_peer(peer_id).to_chat() {
        let mut added = 0usize;
        for &mid in member_ids {
            if let Some(uid) = input_user(live, mid) {
                let tl_enums::messages::InvitedUsers::InvitedUsers(resp) = invoke_rpc(
                    live,
                    tl_fns::messages::AddChatUser {
                        chat_id: chat_id.0 as i64,
                        user_id: uid,
                        fwd_limit: 50,
                    },
                )?;
                let _ = apply_updates_users_chats(live, &resp.updates);
                added += 1;
            }
        }
        live.bridge_state().notify_ui();
        return Ok(added);
    }
    Err("Peer is not a group or channel.".to_owned())
}

/// Kicks/removes a member via `channels.editBanned` (supergroups/channels) or
/// `messages.deleteChatUser` (basic groups).
///
/// Matches `ChannelData::KickedRestrictedRights` (`data/data_channel.cpp:495-514`) and
/// `RestrictionsToMTP` (`data/data_chat_participant_status.cpp:147-177`):
/// - User peers get `ChatBannedRights::KICKED_USER` serialized without legacy
///   `send_messages`/`send_media` bits and `until_date = 0`.
/// - Non-user peers (e.g. a channel posting as itself) get `VIEW_MESSAGES` only.
pub(crate) fn remove_member(
    live: &mut LiveSession,
    peer_id: PeerId,
    user_id: PeerId,
) -> Result<(), String> {
    if let Some(channel) = input_channel(live, peer_id) {
        let participant = input_peer(live, user_id)
            .ok_or_else(|| "Participant peer not found in session.".to_owned())?;
        // Match `ChannelData::KickedRestrictedRights` (`data/data_channel.cpp:495-514`):
        // non-user peers receive `VIEW_MESSAGES` only; `build_edit_banned` uses
        // `tg_api::convert::banned_rights_to_tl` which never sets legacy `send_messages`/`send_media`.
        let restrictions = tg_api::channels::kicked_restrictions_for_peer(to_data_peer(user_id));
        let req = tg_api::channels::build_edit_banned(channel, participant, restrictions);
        let upd = invoke_rpc(live, req)?;
        let _ = apply_updates_users_chats(live, &upd);
        live.bridge_state().notify_ui();
        return Ok(());
    }
    if let Some(chat_id) = to_data_peer(peer_id).to_chat() {
        let target_user =
            input_user(live, user_id).ok_or_else(|| "Member target must be a user.".to_owned())?;
        let upd = invoke_rpc(
            live,
            tl_fns::messages::DeleteChatUser {
                revoke_history: false,
                chat_id: chat_id.0 as i64,
                user_id: target_user,
            },
        )?;
        let _ = apply_updates_users_chats(live, &upd);
        live.bridge_state().notify_ui();
        return Ok(());
    }
    Err("Peer is not a group or channel.".to_owned())
}

/// Updates chat auto-delete timer via `messages.setHistoryTTL`.
pub(crate) fn set_auto_delete_ttl(
    live: &mut LiveSession,
    peer_id: PeerId,
    ttl_seconds: u32,
) -> Result<(), String> {
    let ip = input_peer(live, peer_id).ok_or_else(|| "Peer not found in session.".to_owned())?;
    let upd = invoke_rpc(
        live,
        tl_fns::messages::SetHistoryTTL {
            peer: ip,
            period: ttl_seconds as i32,
        },
    )?;
    let _ = apply_updates_users_chats(live, &upd);
    let data_peer = to_data_peer(peer_id);
    write_session(live, |s| {
        if let Some(p) = s.peer_mut(data_peer) {
            p.common_mut().messages_ttl = ttl_seconds as i32;
        }
    });
    live.bridge_state().notify_ui();
    Ok(())
}

/// Links or unlinks a discussion supergroup via `channels.setDiscussionGroup`.
pub(crate) fn set_linked_discussion(
    live: &mut LiveSession,
    channel_id: PeerId,
    group_id: Option<PeerId>,
) -> Result<(), String> {
    let broadcast = input_channel(live, channel_id)
        .ok_or_else(|| "Broadcast channel not found in session.".to_owned())?;
    let group = match group_id {
        Some(gid) => input_channel(live, gid)
            .ok_or_else(|| "Discussion supergroup not found in session.".to_owned())?,
        None => tl_enums::InputChannel::Empty,
    };
    let _ = invoke_rpc(
        live,
        tl_fns::channels::SetDiscussionGroup { broadcast, group },
    )?;
    let data_ch = to_data_peer(channel_id);
    let linked_cid = group_id.and_then(|g| to_data_peer(g).to_channel());
    write_session(live, |s| {
        if let Some(Peer::Channel(ch)) = s.peer_mut(data_ch) {
            ch.discussion_link = linked_cid;
            ch.discussion_link_known = true;
        }
    });
    live.bridge_state().notify_ui();
    Ok(())
}

/// Updates a peer's public username (`account.updateUsername` for self,
/// `channels.updateUsername` for channels/supergroups).
pub(crate) fn update_username(
    live: &mut LiveSession,
    peer_id: PeerId,
    username: &str,
) -> Result<(), String> {
    let data_peer = to_data_peer(peer_id);
    let is_self = read_session(live, |s| s.self_peer_id() == data_peer);
    if is_self {
        return SettingsBackend::update_username(live, username);
    }
    let channel = input_channel(live, peer_id)
        .ok_or_else(|| "Only channels and supergroups support public usernames.".to_owned())?;
    let cleaned = username.trim().trim_start_matches('@').to_owned();
    let _ = invoke_rpc(
        live,
        tl_fns::channels::UpdateUsername {
            channel,
            username: cleaned.clone(),
        },
    )?;
    write_session(live, |s| {
        if let Some(Peer::Channel(ch)) = s.peer_mut(data_peer) {
            let current_title = ch.common.name.clone();
            ch.set_name(current_title, cleaned);
        }
    });
    live.bridge_state().notify_ui();
    Ok(())
}

// -----------------------------------------------------------------------------
// Additional public admin helpers on LiveSession
// -----------------------------------------------------------------------------

impl LiveSession {
    /// Updates a group or channel's profile photo via `channels.editPhoto` / `messages.editChatPhoto`.
    pub fn edit_chat_photo(
        &mut self,
        peer_id: PeerId,
        photo: tl_enums::InputChatPhoto,
    ) -> Result<(), String> {
        let upd = if let Some(channel) = input_channel(self, peer_id) {
            invoke_rpc(self, tl_fns::channels::EditPhoto { channel, photo })?
        } else if let Some(chat_id) = to_data_peer(peer_id).to_chat() {
            invoke_rpc(
                self,
                tl_fns::messages::EditChatPhoto {
                    chat_id: chat_id.0 as i64,
                    photo,
                },
            )?
        } else {
            return Err("Peer is not a group or channel.".to_owned());
        };
        let _ = apply_updates_users_chats(self, &upd);
        self.bridge_state().notify_ui();
        Ok(())
    }

    /// Updates a channel/supergroup member's banned/restricted rights via `channels.editBanned`.
    pub fn edit_banned_rights(
        &mut self,
        peer_id: PeerId,
        participant_id: PeerId,
        rights: ChatBannedRights,
        until_date: i32,
    ) -> Result<(), String> {
        let channel = input_channel(self, peer_id)
            .ok_or_else(|| "Channel/supergroup not found in session.".to_owned())?;
        let participant = input_peer(self, participant_id)
            .ok_or_else(|| "Participant peer not found in session.".to_owned())?;
        let restrictions = ChatRestrictionsInfo::new(
            rights,
            tg_api::channels::normalize_ban_until_date(until_date),
        );
        let req = tg_api::channels::build_edit_banned(channel, participant, restrictions);
        let upd = invoke_rpc(self, req)?;
        let _ = apply_updates_users_chats(self, &upd);
        self.bridge_state().notify_ui();
        Ok(())
    }

    /// Returns the most recent [`ExportControlHandle`] created by [`SettingsBackend::request_export`].
    #[must_use]
    pub fn export_handle(&self) -> Option<ExportControlHandle> {
        with_admin_state(self, |st| st.export_handle.clone())
    }
}

// -----------------------------------------------------------------------------
// SettingsBackend 2FA cloud password helpers (`tg_api::account::CloudPasswordManager`)
// -----------------------------------------------------------------------------

pub(crate) fn cloud_password_state_from_api(
    pwd: &tg_api::CloudPasswordState,
    unconfirmed_code_length: i32,
) -> CloudPasswordState {
    CloudPasswordState {
        has_password: pwd.has_password,
        has_recovery: pwd.has_recovery,
        hint: pwd.hint.clone(),
        email_pattern: pwd.unconfirmed_pattern.clone(),
        unconfirmed_email_pattern: pwd.unconfirmed_pattern.clone(),
        unconfirmed_code_length,
        login_email_pattern: pwd.login_email_pattern.clone(),
        pending_reset_date: pwd.pending_reset_date,
    }
}

pub(crate) fn cloud_password_state_from_tl(
    pwd: &tl_types::account::Password,
) -> CloudPasswordState {
    let api_state = tg_api::CloudPasswordState::from_account_password(pwd);
    cloud_password_state_from_api(&api_state, 0)
}

fn map_cloud_password_api_error(err: tg_api::ApiError) -> CloudPasswordError {
    match err {
        tg_api::ApiError::ServerError(msg) if msg == "SUGGEST_SECRET_RESET" => {
            CloudPasswordError::SuggestSecretReset
        }
        tg_api::ApiError::Invocation(tg_api::InvocationError::Rpc(rpc_err)) => {
            match rpc_err.name.as_str() {
                "EMAIL_INVALID" => CloudPasswordError::InvalidEmail,
                "CODE_INVALID" | "CODE_EMPTY" | "EMAIL_HASH_EXPIRED" => {
                    CloudPasswordError::InvalidCode
                }
                "PASSWORD_HASH_INVALID" | "SRP_PASSWORD_CHANGED" | "SRP_ID_INVALID" => {
                    CloudPasswordError::WrongPassword
                }
                "PASSWORD_EMPTY" => CloudPasswordError::EmptyPassword,
                "SUGGEST_SECRET_RESET" => CloudPasswordError::SuggestSecretReset,
                _ => CloudPasswordError::Server(rpc_err.name),
            }
        }
        tg_api::ApiError::ServerError(msg) => CloudPasswordError::Server(msg),
        _ => CloudPasswordError::WrongPassword,
    }
}

fn compute_current_srp_check(
    pwd: &tl_types::account::Password,
    password: &str,
) -> Result<tl_enums::InputCheckPasswordSRP, CloudPasswordError> {
    if !pwd.has_password {
        return Ok(tl_enums::InputCheckPasswordSRP::InputCheckPasswordEmpty);
    }
    let algo = pwd
        .current_algo
        .as_ref()
        .and_then(parse_password_kdf_algo)
        .ok_or(CloudPasswordError::WrongPassword)?;
    let srp_b = pwd
        .srp_b
        .as_deref()
        .ok_or(CloudPasswordError::WrongPassword)?;
    let srp_id = pwd.srp_id.ok_or(CloudPasswordError::WrongPassword)?;
    let check = compute_check(&algo, srp_b, srp_id, password.as_bytes())
        .map_err(|_| CloudPasswordError::WrongPassword)?;
    Ok(srp_check_to_tl(&check))
}

/// Verifies the user's current 2FA cloud password via `account.getPassword` + SRP +
/// `account.getPasswordSettings` (`CloudPassword::check`, `api/api_cloud_password.cpp:332-353`).
pub(crate) fn verify_cloud_password(
    live: &LiveSession,
    password: &str,
) -> Result<(), CloudPasswordError> {
    let rpc = live.runtime().rpc().clone();
    let password_owned = password.to_owned();
    let res = block_on_runtime(live.runtime().tokio_handle(), async move {
        tokio::time::timeout(ADMIN_RPC_TIMEOUT, async move {
            let mut mgr = CloudPasswordManager::new();
            mgr.check_password(&rpc, &password_owned).await
        })
        .await
    });
    match res {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(err)) => Err(map_cloud_password_api_error(err)),
        Err(_) => Err(CloudPasswordError::WrongPassword),
    }
}

/// Sets or changes the 2FA cloud password via `tg_api::account::CloudPasswordManager::set_password`
/// (`CloudPassword::set`, `api/api_cloud_password.cpp:187-330`).
///
/// When changing an existing password, fetches `account.getPasswordSettings` to decrypt and
/// re-encrypt Passport `secure_settings` (returning [`CloudPasswordError::SuggestSecretReset`] on
/// decryption/ID mismatch), then fetches a fresh `account.getPassword` (`api_cloud_password.cpp:306-315`)
/// so `account.updatePasswordSettings` uses an unconsumed `srp_id`, and propagates
/// `EMAIL_UNCONFIRMED_<N>` into `CloudPasswordState::unconfirmed_code_length`.
pub(crate) fn set_cloud_password(
    live: &mut LiveSession,
    current_password: Option<&str>,
    new_password: &str,
    hint: &str,
    recovery_email: Option<&str>,
) -> Result<CloudPasswordState, CloudPasswordError> {
    if new_password.is_empty() {
        return Err(CloudPasswordError::EmptyPassword);
    }
    if !hint.is_empty() && hint == new_password {
        return Err(CloudPasswordError::HintMatchesPassword);
    }
    let cleaned_email = recovery_email.map(str::trim).filter(|e| !e.is_empty());
    if let Some(email) = cleaned_email
        && (!email.contains('@') || !email.contains('.'))
    {
        return Err(CloudPasswordError::InvalidEmail);
    }

    let rpc = live.runtime().rpc().clone();
    let old_pwd = current_password.unwrap_or("").to_owned();
    let new_pwd = new_password.to_owned();
    let hint_owned = hint.to_owned();
    let has_email = cleaned_email.is_some();
    let email_owned = cleaned_email.unwrap_or("").to_owned();

    let res = block_on_runtime(live.runtime().tokio_handle(), async move {
        tokio::time::timeout(ADMIN_RPC_TIMEOUT, async move {
            let mut mgr = CloudPasswordManager::new();
            mgr.set_password(
                &rpc,
                &old_pwd,
                &new_pwd,
                &hint_owned,
                has_email,
                &email_owned,
            )
            .await
        })
        .await
    });

    let outcome = match res {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(err)) => return Err(map_cloud_password_api_error(err)),
        Err(_) => return Err(CloudPasswordError::WrongPassword),
    };
    let refreshed = cloud_password_state_from_api(&outcome.state, outcome.unconfirmed_code_length);
    live.bridge_state().notify_ui();
    Ok(refreshed)
}

/// Updates only the 2FA recovery email (`account.updatePasswordSettings` with `flags = 0x02`,
/// `CloudPassword::setEmail`, `api/api_cloud_password.cpp:395-438`).
pub(crate) fn set_recovery_email(
    live: &mut LiveSession,
    current_password: &str,
    recovery_email: &str,
) -> Result<CloudPasswordState, CloudPasswordError> {
    let cleaned = recovery_email.trim();
    if !cleaned.is_empty() && (!cleaned.contains('@') || !cleaned.contains('.')) {
        return Err(CloudPasswordError::InvalidEmail);
    }
    let rpc = live.runtime().rpc().clone();
    let old_pwd = current_password.to_owned();
    let email_owned = cleaned.to_owned();
    let res = block_on_runtime(live.runtime().tokio_handle(), async move {
        tokio::time::timeout(ADMIN_RPC_TIMEOUT, async move {
            let mut mgr = CloudPasswordManager::new();
            mgr.set_recovery_email(&rpc, &old_pwd, &email_owned, None)
                .await
        })
        .await
    });
    let outcome = match res {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(err)) => return Err(map_cloud_password_api_error(err)),
        Err(_) => return Err(CloudPasswordError::WrongPassword),
    };
    let refreshed = cloud_password_state_from_api(&outcome.state, outcome.unconfirmed_code_length);
    live.bridge_state().notify_ui();
    Ok(refreshed)
}

/// Confirms a pending 2FA recovery email via `account.confirmPasswordEmail`
/// (`CloudPassword::confirmEmail`, `api/api_cloud_password.cpp:355-374`).
pub(crate) fn confirm_recovery_email(
    live: &mut LiveSession,
    code: &str,
) -> Result<CloudPasswordState, CloudPasswordError> {
    let trimmed = code.trim();
    if trimmed.is_empty() {
        return Err(CloudPasswordError::InvalidCode);
    }
    let rpc = live.runtime().rpc().clone();
    let code_owned = trimmed.to_owned();
    let res = block_on_runtime(live.runtime().tokio_handle(), async move {
        tokio::time::timeout(ADMIN_RPC_TIMEOUT, async move {
            let mut mgr = CloudPasswordManager::new();
            mgr.confirm_password_email(&rpc, &code_owned).await
        })
        .await
    });
    let api_state = match res {
        Ok(Ok(state)) => state,
        Ok(Err(err)) => return Err(map_cloud_password_api_error(err)),
        Err(_) => return Err(CloudPasswordError::InvalidCode),
    };
    let refreshed = cloud_password_state_from_api(&api_state, 0);
    live.bridge_state().notify_ui();
    Ok(refreshed)
}

/// Resends the 2FA recovery email confirmation code via `account.resendPasswordEmail`
/// (`CloudPassword::resendEmailCode`, `api/api_cloud_password.cpp:376-393`).
pub(crate) fn resend_recovery_email(
    live: &mut LiveSession,
) -> Result<CloudPasswordState, CloudPasswordError> {
    let rpc = live.runtime().rpc().clone();
    let res = block_on_runtime(live.runtime().tokio_handle(), async move {
        tokio::time::timeout(ADMIN_RPC_TIMEOUT, async move {
            let mut mgr = CloudPasswordManager::new();
            mgr.resend_password_email(&rpc).await
        })
        .await
    });
    let api_state = match res {
        Ok(Ok(state)) => state,
        Ok(Err(err)) => return Err(map_cloud_password_api_error(err)),
        Err(_) => return Err(CloudPasswordError::Server("TIMEOUT".to_owned())),
    };
    let refreshed = cloud_password_state_from_api(&api_state, 0);
    live.bridge_state().notify_ui();
    Ok(refreshed)
}

/// Cancels a pending unconfirmed 2FA recovery email via `account.cancelPasswordEmail`
/// (`CloudPassword::cancelEmail`, `api/api_cloud_password.cpp:109-126`).
pub(crate) fn cancel_recovery_email(live: &mut LiveSession) -> CloudPasswordState {
    let rpc = live.runtime().rpc().clone();
    let res = block_on_runtime(live.runtime().tokio_handle(), async move {
        tokio::time::timeout(ADMIN_RPC_TIMEOUT, async move {
            let mut mgr = CloudPasswordManager::new();
            mgr.cancel_password_email(&rpc).await
        })
        .await
    });
    let state = match res {
        Ok(Ok(api_state)) => cloud_password_state_from_api(&api_state, 0),
        Ok(Err(err)) => {
            warn!(error = %err, "account.cancelPasswordEmail / reload failed");
            CloudPasswordState::default()
        }
        Err(_) => CloudPasswordState::default(),
    };
    live.bridge_state().notify_ui();
    state
}

/// Disables the 2FA cloud password via `account.updatePasswordSettings` with empty new hash
/// (`CloudPassword::set` with `newPassword == ""`, `api/api_cloud_password.cpp:187-257`).
pub(crate) fn disable_cloud_password(
    live: &mut LiveSession,
    current_password: &str,
) -> Result<CloudPasswordState, CloudPasswordError> {
    let rpc = live.runtime().rpc().clone();
    let cur_pwd = current_password.to_owned();
    let res = block_on_runtime(live.runtime().tokio_handle(), async move {
        tokio::time::timeout(ADMIN_RPC_TIMEOUT, async move {
            let mut mgr = CloudPasswordManager::new();
            mgr.set_password(&rpc, &cur_pwd, "", "", false, "").await
        })
        .await
    });
    let outcome = match res {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(err)) => return Err(map_cloud_password_api_error(err)),
        Err(_) => return Err(CloudPasswordError::WrongPassword),
    };
    let refreshed = cloud_password_state_from_api(&outcome.state, outcome.unconfirmed_code_length);
    live.bridge_state().notify_ui();
    Ok(refreshed)
}

/// Initiates or completes a wait-period 2FA password reset via `account.resetPassword`
/// (`CloudPassword::resetPassword`, `api/api_cloud_password.cpp:141-170`).
pub(crate) fn reset_cloud_password(
    live: &mut LiveSession,
) -> Result<CloudPasswordResetOutcome, CloudPasswordError> {
    let rpc = live.runtime().rpc().clone();
    let res = block_on_runtime(live.runtime().tokio_handle(), async move {
        tokio::time::timeout(ADMIN_RPC_TIMEOUT, async move {
            let mut mgr = CloudPasswordManager::new();
            mgr.reset_password(&rpc).await
        })
        .await
    });
    let outcome = match res {
        Ok(Ok(ResetPasswordOutcome::ResetOk { state })) => {
            CloudPasswordResetOutcome::ResetOk(cloud_password_state_from_api(&state, 0))
        }
        Ok(Ok(ResetPasswordOutcome::RequestedWait { until_date })) => {
            CloudPasswordResetOutcome::RequestedWait { until_date }
        }
        Ok(Ok(ResetPasswordOutcome::FailedWait { retry_date })) => {
            CloudPasswordResetOutcome::FailedWait { retry_date }
        }
        Ok(Err(err)) => return Err(map_cloud_password_api_error(err)),
        Err(_) => return Err(CloudPasswordError::Server("TIMEOUT".to_owned())),
    };
    live.bridge_state().notify_ui();
    Ok(outcome)
}

/// Declines a pending 2FA password reset via `account.declinePasswordReset`
/// (`CloudPassword::cancelResetPassword`, `api/api_cloud_password.cpp:172-185`).
pub(crate) fn decline_cloud_password_reset(
    live: &mut LiveSession,
) -> Result<CloudPasswordState, CloudPasswordError> {
    let rpc = live.runtime().rpc().clone();
    let res = block_on_runtime(live.runtime().tokio_handle(), async move {
        tokio::time::timeout(ADMIN_RPC_TIMEOUT, async move {
            let mut mgr = CloudPasswordManager::new();
            mgr.decline_password_reset(&rpc).await
        })
        .await
    });
    let api_state = match res {
        Ok(Ok(state)) => state,
        Ok(Err(err)) => return Err(map_cloud_password_api_error(err)),
        Err(_) => return Err(CloudPasswordError::Server("TIMEOUT".to_owned())),
    };
    let refreshed = cloud_password_state_from_api(&api_state, 0);
    live.bridge_state().notify_ui();
    Ok(refreshed)
}

// -----------------------------------------------------------------------------
// SettingsBackend Data Export kickoff (routing to tg-export's TlExportSource)
// -----------------------------------------------------------------------------

/// Adapter bridging `tg-api`'s [`RpcClient`] into `tg-export`'s [`ExportRpc`] trait.
#[derive(Clone)]
pub struct LiveExportRpc<C: RpcClient + Clone + 'static> {
    rpc: C,
}

impl<C: RpcClient + Clone + 'static> LiveExportRpc<C> {
    /// Creates a new [`LiveExportRpc`] wrapping `rpc`.
    #[must_use]
    pub fn new(rpc: C) -> Self {
        Self { rpc }
    }
}

impl<C: RpcClient + Clone + 'static> ExportRpc for LiveExportRpc<C> {
    async fn invoke<R: RemoteCall + Identifiable + Send + Sync>(
        &self,
        dc_id: i32,
        request: &R,
    ) -> Result<R::Return, RpcCallError> {
        let res = if dc_id > 0 {
            self.rpc.invoke_in_dc(dc_id, request).await
        } else {
            self.rpc.invoke(request).await
        };
        res.map_err(|err| {
            let inv: tg_api::rpc::InvocationError = err.into();
            match inv {
                tg_api::rpc::InvocationError::Rpc(rpc_err) => {
                    let msg = format!("RPC {} {}", rpc_err.code, rpc_err.name);
                    RpcCallError::new(rpc_err.code, rpc_err.name, msg)
                }
                other => RpcCallError::new(-1, "TRANSPORT_ERROR", other.to_string()),
            }
        })
    }
}

/// Converts a UI [`ExportRequest`] into `tg-export`'s [`ExportSettings`].
#[must_use]
pub(crate) fn export_request_to_settings(request: &ExportRequest) -> ExportSettings {
    let mut types = ExportTypes::empty();
    if request.account_info {
        types |= ExportTypes::PERSONAL_INFO | ExportTypes::CONTACTS;
    }
    if request.personal_chats {
        types |= ExportTypes::PERSONAL_CHATS | ExportTypes::BOT_CHATS;
    }
    if request.groups_and_channels {
        types |= ExportTypes::GROUPS_CHANNELS_MASK;
    }
    if types.is_empty() {
        types = ExportTypes::PERSONAL_INFO;
    }

    let media = MediaSettings {
        types: if request.include_media {
            MediaTypes::ALL_MASK
        } else {
            MediaTypes::empty()
        },
        ..MediaSettings::default()
    };

    let format = match request.format {
        ExportFormat::Html => OutputFormat::Html,
        ExportFormat::Json => OutputFormat::Json,
        ExportFormat::HtmlAndJson => OutputFormat::HtmlAndJson,
    };

    let path = if request.path.trim().is_empty() {
        "Downloads/Rustgram Export".to_owned()
    } else {
        request.path.trim().to_owned()
    };

    ExportSettings {
        path,
        force_sub_path: false,
        format,
        types,
        media,
        ..ExportSettings::default()
    }
}

/// Kicks off an asynchronous Telegram data export routed through `tg-export`'s
/// [`TlExportSource`] and [`ExportController`] on the session's Tokio runtime.
pub(crate) fn request_export(live: &mut LiveSession, request: &ExportRequest) -> String {
    let settings = export_request_to_settings(request);
    let format_label = match request.format {
        ExportFormat::Html => "HTML",
        ExportFormat::Json => "JSON",
        ExportFormat::HtmlAndJson => "HTML + JSON",
    };
    let display_path = settings.path.clone();

    let source = TlExportSource::new(LiveExportRpc::new(live.runtime().rpc().clone()));
    let (mut controller, handle) =
        ExportController::new(source, tg_export::stand_in::InputPeer::Empty);
    controller.set_skip_path_normalization(true);

    with_admin_state(live, |st| {
        if let Some(prev) = st.export_handle.replace(handle.clone()) {
            prev.cancel_fast();
        }
    });

    let env = Environment {
        internal_links_domain: "https://t.me/".to_owned(),
        ..Environment::default()
    };
    let bridge = Arc::clone(live.bridge_state());
    live.runtime().tokio_handle().spawn(async move {
        let _final_state = controller.run(&settings, &env).await;
        bridge.notify_ui();
    });

    let _self_uid: UserId = read_session(live, Session::self_user_id);
    format!("Data export started ({format_label}) to {display_path}")
}

// -----------------------------------------------------------------------------
// WP-10: Invite Links, Join Requests, Peer Colors, Ownership Transfer,
// Similar Channels, and Downloads Manager
// -----------------------------------------------------------------------------

fn parse_exported_invite(
    session: &Session,
    invite: &tl_enums::ExportedChatInvite,
) -> Option<InviteLinkInfo> {
    match invite {
        tl_enums::ExportedChatInvite::ChatInviteExported(inv) => {
            let admin_data_peer = DataPeerId::from_user(UserId::from_tl(inv.admin_id));
            let admin_name = session
                .peer(admin_data_peer)
                .map(|p| p.common().name.clone())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| format!("Admin {}", inv.admin_id));
            Some(InviteLinkInfo {
                link: inv.link.clone(),
                title: inv.title.clone().unwrap_or_default(),
                admin_id: from_data_peer(admin_data_peer),
                admin_name,
                date: inv.date,
                start_date: inv.start_date,
                expire_date: inv.expire_date,
                usage_limit: inv.usage_limit,
                usage: inv.usage.unwrap_or(0),
                requested: inv.requested.unwrap_or(0),
                permanent: inv.permanent,
                revoked: inv.revoked,
                request_needed: inv.request_needed,
            })
        }
        tl_enums::ExportedChatInvite::ChatInvitePublicJoinRequests => None,
    }
}

fn self_input_user(live: &LiveSession) -> tl_enums::InputUser {
    let self_uid = read_session(live, Session::self_user_id);
    if self_uid.0 == 0 {
        tl_enums::InputUser::Self_
    } else {
        read_session(live, |s| {
            let iu = input_user_for(s, self_uid);
            if matches!(iu, tl_enums::InputUser::Empty) {
                tl_enums::InputUser::Self_
            } else {
                iu
            }
        })
    }
}

/// Maximum number of pages drained in a single cursor-paginated invite links or
/// importers fetch to guard against looping responses.
const MAX_INVITE_PAGINATION_PAGES: usize = 100;

/// Lists active (`revoked = false`) or revoked (`revoked = true`) invite links via
/// `messages.getExportedChatInvites`, paginating with `(offset_date, offset_link)` cursors
/// matching `Api::InviteLinks::requestMyLinks` / `requestMoreLinks`
/// (`api/api_invite_links.cpp:24-25, 415-456, 772-793`).
pub(crate) fn get_exported_chat_invites(
    live: &LiveSession,
    peer_id: PeerId,
    revoked: bool,
) -> Result<InviteLinksList, String> {
    let data_peer = to_data_peer(peer_id);
    let _ = input_peer(live, peer_id).ok_or_else(|| "Peer not found in session.".to_owned())?;
    let admin_id = read_session(live, Session::self_user_id);

    let mut total_count: i32 = 0;
    let mut links: Vec<InviteLinkInfo> = Vec::new();
    let mut offset: Option<(i32, String)> = None;

    for _ in 0..MAX_INVITE_PAGINATION_PAGES {
        // Match `Api::InviteLinks::requestMyLinks` (`kFirstPage = 10`, no offset) and
        // `requestMoreLinks` (`kPerPage = 50`, `offset_date` + `offset_link` shared flag)
        // in `api/api_invite_links.cpp:415-426, 772-793`.
        let query = tg_api::channels::ExportedChatInvitesQuery {
            admin_id,
            revoked,
            offset: offset.clone(),
            limit: 0,
        };
        let mut req = read_session(live, |s| {
            tg_api::channels::build_get_exported_chat_invites(s, data_peer, &query)
        });
        if matches!(req.admin_id, tl_enums::InputUser::Empty) {
            req.admin_id = tl_enums::InputUser::Self_;
        }
        let tl_enums::messages::ExportedChatInvites::ExportedChatInvites(resp) =
            invoke_rpc(live, req)?;
        write_session(live, |s| {
            apply_tl_users_and_chats(s, &resp.users, &[]);
        });
        total_count = total_count.max(resp.count);
        let page_links = read_session(live, |s| {
            resp.invites
                .iter()
                .filter_map(|inv| parse_exported_invite(s, inv))
                .collect::<Vec<_>>()
        });
        if page_links.is_empty() {
            break;
        }
        let next_offset = page_links.last().map(|last| (last.date, last.link.clone()));
        links.extend(page_links);
        if links.len() as i32 >= total_count || next_offset.is_none() || next_offset == offset {
            break;
        }
        offset = next_offset;
    }

    Ok(InviteLinksList {
        count: total_count.max(links.len() as i32),
        links,
    })
}

/// Creates a new invite link via `messages.exportChatInvite`.
pub(crate) fn export_chat_invite(
    live: &mut LiveSession,
    peer_id: PeerId,
    req: InviteLinkRequest,
) -> Result<InviteLinkInfo, String> {
    let peer = input_peer(live, peer_id).ok_or_else(|| "Peer not found in session.".to_owned())?;
    let title = req.title.trim().to_owned();
    let usage_limit = if req.request_needed {
        None
    } else {
        req.usage_limit.filter(|&n| n > 0)
    };
    let resp = invoke_rpc(
        live,
        tl_fns::messages::ExportChatInvite {
            legacy_revoke_permanent: req.legacy_revoke_permanent,
            request_needed: req.request_needed,
            peer,
            expire_date: req.expire_date.filter(|&d| d > 0),
            usage_limit,
            title: (!title.is_empty()).then_some(title),
            subscription_pricing: None,
        },
    )?;
    let parsed = read_session(live, |s| parse_exported_invite(s, &resp))
        .ok_or_else(|| "Server returned an unsupported invite link variant.".to_owned())?;
    if parsed.permanent {
        let data_peer = to_data_peer(peer_id);
        let url = parsed.link.clone();
        write_session(live, |s| match s.peer_mut(data_peer) {
            Some(Peer::Channel(ch)) => ch.invite_link = url,
            Some(Peer::Chat(chat)) => chat.invite_link = url,
            _ => {}
        });
    }
    live.bridge_state().notify_ui();
    Ok(parsed)
}

/// Edits an existing invite link via `messages.editExportedChatInvite`.
pub(crate) fn edit_exported_chat_invite(
    live: &mut LiveSession,
    peer_id: PeerId,
    link: &str,
    req: InviteLinkRequest,
) -> Result<InviteLinkInfo, String> {
    let peer = input_peer(live, peer_id).ok_or_else(|| "Peer not found in session.".to_owned())?;
    let usage_limit = if req.request_needed {
        None
    } else {
        Some(req.usage_limit.unwrap_or(0))
    };
    let request_needed = if req.request_needed || req.usage_limit.unwrap_or(0) == 0 {
        Some(req.request_needed)
    } else {
        None
    };
    let resp = invoke_rpc(
        live,
        tl_fns::messages::EditExportedChatInvite {
            revoked: false,
            peer,
            link: link.to_owned(),
            expire_date: Some(req.expire_date.unwrap_or(0)),
            usage_limit,
            request_needed,
            title: Some(req.title.trim().to_owned()),
        },
    )?;
    let (invite_tl, users) = match resp {
        tl_enums::messages::ExportedChatInvite::ExportedChatInvite(d) => (d.invite, d.users),
        tl_enums::messages::ExportedChatInvite::Replaced(d) => (d.invite, d.users),
    };
    write_session(live, |s| {
        apply_tl_users_and_chats(s, &users, &[]);
    });
    let parsed = read_session(live, |s| parse_exported_invite(s, &invite_tl))
        .ok_or_else(|| "Server returned an unsupported edited invite link.".to_owned())?;
    live.bridge_state().notify_ui();
    Ok(parsed)
}

/// Revokes an invite link via `messages.editExportedChatInvite(revoked: true)`.
///
/// When revoking the permanent link, Telegram returns `messages.exportedChatInviteReplaced`
/// containing both the revoked link and the newly generated permanent replacement link.
pub(crate) fn revoke_exported_chat_invite(
    live: &mut LiveSession,
    peer_id: PeerId,
    link: &str,
) -> Result<(InviteLinkInfo, Option<InviteLinkInfo>), String> {
    let peer = input_peer(live, peer_id).ok_or_else(|| "Peer not found in session.".to_owned())?;
    let resp = invoke_rpc(
        live,
        tl_fns::messages::EditExportedChatInvite {
            revoked: true,
            peer,
            link: link.to_owned(),
            expire_date: None,
            usage_limit: None,
            request_needed: None,
            title: None,
        },
    )?;
    let (revoked_tl, replacement_tl, users) = match resp {
        tl_enums::messages::ExportedChatInvite::ExportedChatInvite(d) => (d.invite, None, d.users),
        tl_enums::messages::ExportedChatInvite::Replaced(d) => {
            (d.invite, Some(d.new_invite), d.users)
        }
    };
    write_session(live, |s| {
        apply_tl_users_and_chats(s, &users, &[]);
    });
    let (revoked_info, replacement_info) = read_session(live, |s| {
        (
            parse_exported_invite(s, &revoked_tl),
            replacement_tl
                .as_ref()
                .and_then(|inv| parse_exported_invite(s, inv)),
        )
    });
    let revoked_info = revoked_info
        .ok_or_else(|| "Server returned an unsupported revoked invite link.".to_owned())?;
    if let Some(ref fresh) = replacement_info
        && fresh.permanent
    {
        let data_peer = to_data_peer(peer_id);
        let url = fresh.link.clone();
        write_session(live, |s| match s.peer_mut(data_peer) {
            Some(Peer::Channel(ch)) => ch.invite_link = url,
            Some(Peer::Chat(chat)) => chat.invite_link = url,
            _ => {}
        });
    }
    live.bridge_state().notify_ui();
    Ok((revoked_info, replacement_info))
}

/// Permanently deletes a single revoked invite link via `messages.deleteExportedChatInvite`.
pub(crate) fn delete_exported_chat_invite(
    live: &mut LiveSession,
    peer_id: PeerId,
    link: &str,
) -> Result<(), String> {
    let peer = input_peer(live, peer_id).ok_or_else(|| "Peer not found in session.".to_owned())?;
    let _ = invoke_rpc(
        live,
        tl_fns::messages::DeleteExportedChatInvite {
            peer,
            link: link.to_owned(),
        },
    )?;
    live.bridge_state().notify_ui();
    Ok(())
}

/// Permanently deletes all revoked invite links via `messages.deleteRevokedExportedChatInvites`.
pub(crate) fn delete_revoked_exported_chat_invites(
    live: &mut LiveSession,
    peer_id: PeerId,
) -> Result<(), String> {
    let peer = input_peer(live, peer_id).ok_or_else(|| "Peer not found in session.".to_owned())?;
    let admin_id = self_input_user(live);
    let _ = invoke_rpc(
        live,
        tl_fns::messages::DeleteRevokedExportedChatInvites { peer, admin_id },
    )?;
    live.bridge_state().notify_ui();
    Ok(())
}

/// Lists joined importers or pending join requests (`requested = true`) via
/// `messages.getChatInviteImporters`, paginating with `(offset_date, offset_user)` cursors
/// matching `Api::InviteLinks::requestJoinedFirstSlice` and `messages.getChatInviteImporters`
/// paging (`api/api_invite_links.cpp:25-26, 598-618`).
pub(crate) fn get_chat_invite_importers(
    live: &LiveSession,
    peer_id: PeerId,
    link: Option<&str>,
    requested: bool,
    query: &str,
) -> Result<InviteImportersList, String> {
    let data_peer = to_data_peer(peer_id);
    let _ = input_peer(live, peer_id).ok_or_else(|| "Peer not found in session.".to_owned())?;
    let cleaned_link = link.filter(|l| !l.is_empty()).map(str::to_owned);
    let trimmed_q = query.trim();
    let cleaned_q = (!trimmed_q.is_empty()).then(|| trimmed_q.to_owned());

    let mut total_count: i32 = 0;
    let mut importers: Vec<InviteImporterInfo> = Vec::new();
    let mut offset: Option<(i32, UserId)> = None;

    for _ in 0..MAX_INVITE_PAGINATION_PAGES {
        let (offset_date, offset_user) = match offset {
            Some((date, uid)) => (date, Some(uid)),
            None => (0, None),
        };
        // Match `Api::InviteLinks::requestJoinedFirstSlice` (`kJoinedFirstPage = 10`,
        // `offset_date = 0`, `offset_user = InputUser::Empty`) and subsequent pages
        // (`kPerPage = 50`, `(last.date, last.user_id)`) in `api/api_invite_links.cpp:25-26, 598-618`.
        let api_query = tg_api::channels::ChatInviteImportersQuery {
            requested,
            subscription_expired: false,
            link: cleaned_link.clone(),
            query: cleaned_q.clone(),
            offset_date,
            offset_user,
            limit: 0,
        };
        let req = read_session(live, |s| {
            tg_api::channels::build_get_chat_invite_importers(s, data_peer, &api_query)
        });
        let tl_enums::messages::ChatInviteImporters::ChatInviteImporters(resp) =
            invoke_rpc(live, req)?;
        write_session(live, |s| {
            apply_tl_users_and_chats(s, &resp.users, &[]);
        });
        total_count = total_count.max(resp.count);
        if resp.importers.is_empty() {
            break;
        }
        let mut next_offset: Option<(i32, UserId)> = None;
        let page_importers = read_session(live, |s| {
            resp.importers
                .iter()
                .map(|imp| {
                    let tl_enums::ChatInviteImporter::ChatInviteImporter(data) = imp;
                    let uid = UserId::from_tl(data.user_id);
                    next_offset = Some((data.date, uid));
                    let data_peer = DataPeerId::from_user(uid);
                    let (name, username, color_index) =
                        match s.peer(data_peer).and_then(Peer::as_user) {
                            Some(u) => (
                                u.common.name.clone(),
                                (!u.usernames.username().is_empty())
                                    .then(|| u.usernames.username().to_owned()),
                                u.common.color_index % 7,
                            ),
                            None => (
                                format!("User {}", data.user_id),
                                None,
                                tg_data::decide_color_index(data_peer) % 7,
                            ),
                        };
                    InviteImporterInfo {
                        user_id: from_data_peer(data_peer),
                        name,
                        username,
                        about: data.about.clone(),
                        date: data.date,
                        requested: data.requested,
                        via_chatlist: data.via_chatlist,
                        approved_by: data
                            .approved_by
                            .map(|id| from_data_peer(DataPeerId::from_user(UserId::from_tl(id)))),
                        color_index,
                    }
                })
                .collect::<Vec<_>>()
        });
        importers.extend(page_importers);
        if importers.len() as i32 >= total_count || next_offset.is_none() || next_offset == offset {
            break;
        }
        offset = next_offset;
    }

    Ok(InviteImportersList {
        count: total_count.max(importers.len() as i32),
        importers,
    })
}

/// Approves (`approved = true`) or dismisses (`approved = false`) a single join request
/// via `messages.hideChatJoinRequest`.
pub(crate) fn hide_chat_join_request(
    live: &mut LiveSession,
    peer_id: PeerId,
    user_id: PeerId,
    approved: bool,
) -> Result<(), String> {
    let peer = input_peer(live, peer_id).ok_or_else(|| "Peer not found in session.".to_owned())?;
    let target_user =
        input_user(live, user_id).ok_or_else(|| "Requester must be a user peer.".to_owned())?;
    let upd = invoke_rpc(
        live,
        tl_fns::messages::HideChatJoinRequest {
            approved,
            peer,
            user_id: target_user,
        },
    )?;
    let _ = apply_updates_users_chats(live, &upd);
    let data_peer = to_data_peer(peer_id);
    let target_uid = to_data_peer(user_id).to_user();
    write_session(live, |s| match s.peer_mut(data_peer) {
        Some(Peer::Channel(ch)) => {
            ch.pending_requests_count = (ch.pending_requests_count - 1).max(0);
            if let Some(uid) = target_uid {
                ch.recent_requesters.retain(|&u| u != uid);
            }
        }
        Some(Peer::Chat(chat)) => {
            chat.pending_requests_count = (chat.pending_requests_count - 1).max(0);
            if let Some(uid) = target_uid {
                chat.recent_requesters.retain(|&u| u != uid);
            }
        }
        _ => {}
    });
    live.bridge_state().notify_ui();
    Ok(())
}

/// Approves or dismisses all pending join requests via `messages.hideAllChatJoinRequests`.
pub(crate) fn hide_all_chat_join_requests(
    live: &mut LiveSession,
    peer_id: PeerId,
    link: Option<&str>,
    approved: bool,
) -> Result<usize, String> {
    let peer = input_peer(live, peer_id).ok_or_else(|| "Peer not found in session.".to_owned())?;
    let data_peer = to_data_peer(peer_id);
    let prev_count = read_session(live, |s| match s.peer(data_peer) {
        Some(Peer::Channel(ch)) => ch.pending_requests_count.max(0) as usize,
        Some(Peer::Chat(chat)) => chat.pending_requests_count.max(0) as usize,
        _ => 0,
    });
    let upd = invoke_rpc(
        live,
        tl_fns::messages::HideAllChatJoinRequests {
            approved,
            peer,
            link: link.filter(|l| !l.is_empty()).map(str::to_owned),
        },
    )?;
    let _ = apply_updates_users_chats(live, &upd);
    if link.is_none() {
        write_session(live, |s| match s.peer_mut(data_peer) {
            Some(Peer::Channel(ch)) => {
                ch.pending_requests_count = 0;
                ch.recent_requesters.clear();
            }
            Some(Peer::Chat(chat)) => {
                chat.pending_requests_count = 0;
                chat.recent_requesters.clear();
            }
            _ => {}
        });
    }
    live.bridge_state().notify_ui();
    Ok(prev_count)
}

/// Reads the in-chat join-requests bar summary from the cached `tg-data::Session`.
pub(crate) fn join_requests_summary(live: &LiveSession, peer_id: PeerId) -> JoinRequestsSummary {
    let data_peer = to_data_peer(peer_id);
    read_session(live, |s| {
        let (count, recent_ids) = match s.peer(data_peer) {
            Some(Peer::Channel(ch)) => (
                ch.pending_requests_count.max(0) as u32,
                ch.recent_requesters.clone(),
            ),
            Some(Peer::Chat(chat)) => (
                chat.pending_requests_count.max(0) as u32,
                chat.recent_requesters.clone(),
            ),
            _ => return JoinRequestsSummary::default(),
        };
        let recent_requesters = recent_ids
            .into_iter()
            .take(3)
            .map(|uid| {
                let up = DataPeerId::from_user(uid);
                let (short_name, color_index) = match s.peer(up).and_then(Peer::as_user) {
                    Some(u) => {
                        let short = if !u.first_name.is_empty() {
                            u.first_name.clone()
                        } else {
                            u.common.name.clone()
                        };
                        (short, u.common.color_index % 7)
                    }
                    None => (
                        format!("User {}", uid.0),
                        tg_data::decide_color_index(up) % 7,
                    ),
                };
                (from_data_peer(up), short_name, color_index)
            })
            .collect();
        JoinRequestsSummary {
            count,
            recent_requesters,
        }
    })
}

/// Reads current reply/name and profile color settings for `peer_id` from `tg-data::Session`.
pub(crate) fn peer_color_settings(live: &LiveSession, peer_id: PeerId) -> PeerColorSettings {
    let data_peer = to_data_peer(peer_id);
    read_session(live, |s| match s.peer(data_peer) {
        Some(p) => {
            let c = p.common();
            PeerColorSettings {
                color_index: Some(c.color_index),
                background_emoji_id: c.background_emoji_id.0,
                profile_color_index: c.color_profile_index,
                profile_background_emoji_id: c.profile_background_emoji_id.0,
            }
        }
        None => PeerColorSettings::default(),
    })
}

/// Updates peer accent or profile color via `account.updateColor` (for self) or
/// `channels.updateColor` (for channels/supergroups).
pub(crate) fn update_peer_color(
    live: &mut LiveSession,
    peer_id: PeerId,
    req: UpdatePeerColorRequest,
) -> Result<(), String> {
    let data_peer = to_data_peer(peer_id);
    let is_self = read_session(live, |s| s.self_peer_id() == data_peer);
    if is_self {
        let color = req.color_index.map(|idx| {
            tl_enums::PeerColor::PeerColor(tl_types::PeerColor {
                color: Some(i32::from(idx)),
                background_emoji_id: (req.background_emoji_id != 0)
                    .then_some(req.background_emoji_id as i64),
            })
        });
        let _ = invoke_rpc(
            live,
            tl_fns::account::UpdateColor {
                for_profile: req.for_profile,
                color,
            },
        )?;
    } else if let Some(channel) = input_channel(live, peer_id) {
        let upd = invoke_rpc(
            live,
            tl_fns::channels::UpdateColor {
                for_profile: req.for_profile,
                channel,
                color: req.color_index.map(i32::from),
                background_emoji_id: Some(req.background_emoji_id as i64),
            },
        )?;
        let _ = apply_updates_users_chats(live, &upd);
    } else {
        return Err(
            "Peer color editing is only supported for your profile and channels.".to_owned(),
        );
    }

    write_session(live, |s| {
        if let Some(p) = s.peer_mut(data_peer) {
            let c = p.common_mut();
            if req.for_profile {
                c.color_profile_index = req.color_index;
                c.profile_background_emoji_id = DocumentId(req.background_emoji_id);
            } else {
                if let Some(idx) = req.color_index {
                    c.color_index = idx;
                    c.color_index_cloud = true;
                }
                c.background_emoji_id = DocumentId(req.background_emoji_id);
            }
        }
    });
    live.bridge_state().notify_ui();
    Ok(())
}

fn parse_transfer_precheck_rpc_error(name: &str) -> Option<OwnershipTransferError> {
    if name == "PASSWORD_MISSING" {
        return Some(OwnershipTransferError::PasswordMissing);
    }
    if let Some(rest) = name.strip_prefix("PASSWORD_TOO_FRESH_") {
        let secs = rest.parse::<u32>().unwrap_or(0);
        return Some(OwnershipTransferError::PasswordTooFresh {
            retry_after_secs: secs,
        });
    }
    if let Some(rest) = name.strip_prefix("SESSION_TOO_FRESH_") {
        let secs = rest.parse::<u32>().unwrap_or(0);
        return Some(OwnershipTransferError::SessionTooFresh {
            retry_after_secs: secs,
        });
    }
    None
}

fn map_transfer_rpc_error(name: &str, is_broadcast: bool) -> OwnershipTransferError {
    if let Some(pre) = parse_transfer_precheck_rpc_error(name) {
        return pre;
    }
    match name {
        "PASSWORD_HASH_INVALID" | "SRP_ID_INVALID" => OwnershipTransferError::WrongPassword,
        "CHANNELS_ADMIN_PUBLIC_TOO_MUCH" => OwnershipTransferError::ServerError(
            "This user already owns too many public channels.".to_owned(),
        ),
        "CHANNELS_ADMIN_LOCATED_TOO_MUCH" => OwnershipTransferError::ServerError(
            "This user already owns too many location-based groups.".to_owned(),
        ),
        "ADMINS_TOO_MUCH" => OwnershipTransferError::ServerError(if is_broadcast {
            "This channel has reached the maximum number of administrators.".to_owned()
        } else {
            "This group has reached the maximum number of administrators.".to_owned()
        }),
        "CHANNEL_INVALID" | "CHAT_CREATOR_REQUIRED" | "PARTICIPANT_MISSING" => {
            OwnershipTransferError::ServerError(if is_broadcast {
                "This channel is no longer accessible for ownership transfer.".to_owned()
            } else {
                "This group is no longer accessible for ownership transfer.".to_owned()
            })
        }
        other => OwnershipTransferError::ServerError(format!("Ownership transfer failed: {other}")),
    }
}

/// Runs Telegram Desktop's ownership-transfer pre-flight check
/// (`boxes/peers/channel_ownership_transfer.cpp:48-77`): calls `messages.editChatCreator`
/// with empty user & empty password to surface `PASSWORD_MISSING` / `PASSWORD_TOO_FRESH_*` /
/// `SESSION_TOO_FRESH_*`, and loads the 2FA password hint via `account.getPassword`.
pub(crate) fn check_ownership_transfer(
    live: &LiveSession,
    peer_id: PeerId,
) -> Result<OwnershipTransferCheck, String> {
    let peer = input_peer(live, peer_id).ok_or_else(|| "Peer not found in session.".to_owned())?;
    let precheck = invoke_rpc_raw(
        live,
        tl_fns::messages::EditChatCreator {
            peer,
            user_id: tl_enums::InputUser::Empty,
            password: tl_enums::InputCheckPasswordSRP::InputCheckPasswordEmpty,
        },
    );
    if let Err(tg_api::rpc::InvocationError::Rpc(rpc_err)) = precheck
        && let Some(reason) = parse_transfer_precheck_rpc_error(&rpc_err.name)
    {
        let message = reason.display_message();
        return Ok(OwnershipTransferCheck::Blocked { reason, message });
    }

    let hint = match invoke_rpc(live, tl_fns::account::GetPassword {}) {
        Ok(tl_enums::account::Password::Password(pwd)) => {
            if !pwd.has_password {
                let reason = OwnershipTransferError::PasswordMissing;
                let message = reason.display_message();
                return Ok(OwnershipTransferCheck::Blocked { reason, message });
            }
            pwd.hint.unwrap_or_default()
        }
        Err(_) => String::new(),
    };

    Ok(OwnershipTransferCheck::Ready {
        password_hint: hint,
    })
}

/// Transfers group/channel ownership to `new_owner_id` using `account.getPassword` +
/// SRP (`tg-crypto::srp`) + `messages.editChatCreator` (Layer 233 successor to
/// `channels.editCreator`).
pub(crate) fn transfer_ownership(
    live: &mut LiveSession,
    peer_id: PeerId,
    new_owner_id: PeerId,
    password: &str,
) -> Result<(), OwnershipTransferError> {
    if password.is_empty() {
        return Err(OwnershipTransferError::WrongPassword);
    }
    let peer = input_peer(live, peer_id).ok_or_else(|| {
        OwnershipTransferError::ServerError("Peer not found in session.".to_owned())
    })?;
    let target_user = input_user(live, new_owner_id).ok_or_else(|| {
        OwnershipTransferError::ServerError("New owner must be a user peer.".to_owned())
    })?;
    let data_peer = to_data_peer(peer_id);
    let is_broadcast = read_session(live, |s| {
        s.peer(data_peer)
            .and_then(Peer::as_channel)
            .is_some_and(|ch| ch.flags.contains(ChannelFlags::BROADCAST))
    });

    let tl_enums::account::Password::Password(pwd) =
        invoke_rpc(live, tl_fns::account::GetPassword {}).map_err(|e| {
            OwnershipTransferError::ServerError(format!("Could not fetch 2FA parameters: {e}"))
        })?;
    if !pwd.has_password {
        return Err(OwnershipTransferError::PasswordMissing);
    }
    let srp_check = compute_current_srp_check(&pwd, password)
        .map_err(|_| OwnershipTransferError::WrongPassword)?;

    match invoke_rpc_raw(
        live,
        tl_fns::messages::EditChatCreator {
            peer,
            user_id: target_user,
            password: srp_check,
        },
    ) {
        Ok(upd) => {
            let _ = apply_updates_users_chats(live, &upd);
            write_session(live, |s| match s.peer_mut(data_peer) {
                Some(Peer::Channel(ch)) => {
                    ch.flags.remove(ChannelFlags::CREATOR);
                }
                Some(Peer::Chat(chat)) => {
                    chat.flags.remove(ChatFlags::CREATOR);
                }
                _ => {}
            });
            live.bridge_state().notify_ui();
            Ok(())
        }
        Err(tg_api::rpc::InvocationError::Rpc(rpc_err)) => {
            Err(map_transfer_rpc_error(&rpc_err.name, is_broadcast))
        }
        Err(other) => Err(OwnershipTransferError::ServerError(format!(
            "Ownership transfer failed: {other}"
        ))),
    }
}

/// Fetches similar recommended channels via `channels.getChannelRecommendations`.
pub(crate) fn get_channel_recommendations(
    live: &LiveSession,
    peer_id: PeerId,
) -> Result<SimilarChannelsResult, String> {
    let channel = input_channel(live, peer_id);
    let resp = invoke_rpc(
        live,
        tl_fns::channels::GetChannelRecommendations { channel },
    )?;
    let (chats, total_count, has_more) = match resp {
        tl_enums::messages::Chats::Chats(c) => {
            let n = c.chats.len() as u32;
            (c.chats, n, false)
        }
        tl_enums::messages::Chats::Slice(s) => {
            let total = s.count.max(0) as u32;
            let more = total > s.chats.len() as u32;
            (s.chats, total, more)
        }
    };
    write_session(live, |s| {
        apply_tl_users_and_chats(s, &[], &chats);
    });
    let entries: Vec<SimilarChannelEntry> = chats
        .iter()
        .filter_map(|chat| match chat {
            tl_enums::Chat::Channel(ch) => {
                let data_pid = DataPeerId::from_channel(ChannelId::from_tl(ch.id));
                let username = ch.username.clone().or_else(|| {
                    ch.usernames.as_ref().and_then(|list| {
                        list.iter().find_map(|u| {
                            let tl_enums::Username::Username(u) = u;
                            u.active.then(|| u.username.clone())
                        })
                    })
                });
                let color_index = ch
                    .color
                    .as_ref()
                    .and_then(|pc| match pc {
                        tl_enums::PeerColor::PeerColor(c) => c.color.map(|v| (v.max(0) as u8) % 7),
                        _ => None,
                    })
                    .unwrap_or_else(|| tg_data::decide_color_index(data_pid) % 7);
                Some(SimilarChannelEntry {
                    peer_id: from_data_peer(data_pid),
                    title: ch.title.clone(),
                    username,
                    subscribers_count: ch.participants_count.unwrap_or(0).max(0) as u32,
                    verified: ch.verified,
                    color_index,
                })
            }
            _ => None,
        })
        .collect();
    Ok(SimilarChannelsResult {
        channels: entries,
        total_count,
        has_more_for_premium: has_more,
    })
}

/// Lists tracked downloads in `LiveSession`'s Downloads Manager filtered by `query`.
pub(crate) fn downloads_list(live: &LiveSession, query: &str) -> Vec<DownloadEntry> {
    let q = query.trim().to_ascii_lowercase();
    with_admin_state(live, |st| {
        st.downloads
            .iter()
            .filter(|d| d.matches_query(&q))
            .cloned()
            .collect()
    })
}

/// Toggles pause/resume on a tracked download in `LiveSession`.
pub(crate) fn toggle_download_pause(
    live: &mut LiveSession,
    download_id: u64,
) -> Result<(), String> {
    let res = with_admin_state(live, |st| {
        let entry = st
            .downloads
            .iter_mut()
            .find(|d| d.id == download_id)
            .ok_or_else(|| "Download not found.".to_owned())?;
        entry.status = match entry.status {
            DownloadStatus::Downloading {
                received_bytes,
                total_bytes,
            } => DownloadStatus::Paused {
                received_bytes,
                total_bytes,
            },
            DownloadStatus::Paused {
                received_bytes,
                total_bytes,
            } => DownloadStatus::Downloading {
                received_bytes,
                total_bytes,
            },
            DownloadStatus::Completed => {
                return Err("Download is already completed.".to_owned());
            }
            DownloadStatus::Failed { .. } => DownloadStatus::Downloading {
                received_bytes: 0,
                total_bytes: entry.total_bytes,
            },
        };
        Ok(())
    });
    if res.is_ok() {
        live.bridge_state().notify_ui();
    }
    res
}

/// Cancels or deletes a tracked download entry (and removes its local file if requested).
pub(crate) fn cancel_or_delete_download(
    live: &mut LiveSession,
    download_id: u64,
    delete_file: bool,
) -> Result<(), String> {
    let removed_path = with_admin_state(live, |st| {
        let idx = st
            .downloads
            .iter()
            .position(|d| d.id == download_id)
            .ok_or_else(|| "Download not found.".to_owned())?;
        let removed = st.downloads.remove(idx);
        Ok::<Option<String>, String>(removed.local_path)
    })?;
    if delete_file && let Some(path) = removed_path {
        let _ = std::fs::remove_file(path);
    }
    live.bridge_state().notify_ui();
    Ok(())
}

/// Clears all completed download entries from `LiveSession`'s Downloads Manager.
pub(crate) fn clear_completed_downloads(live: &mut LiveSession) -> Result<usize, String> {
    let removed = with_admin_state(live, |st| {
        let before = st.downloads.len();
        st.downloads
            .retain(|d| !matches!(d.status, DownloadStatus::Completed));
        before - st.downloads.len()
    });
    live.bridge_state().notify_ui();
    Ok(removed)
}

/// Deletes all download entries from `LiveSession`'s Downloads Manager.
pub(crate) fn delete_all_downloads(live: &mut LiveSession) -> Result<usize, String> {
    let removed = with_admin_state(live, |st| {
        let count = st.downloads.len();
        st.downloads.clear();
        count
    });
    live.bridge_state().notify_ui();
    Ok(removed)
}

impl LiveSession {
    /// Registers a file download in the live Downloads Manager and returns its assigned ID.
    pub fn track_download(&self, mut entry: DownloadEntry) -> u64 {
        let id = with_admin_state(self, |st| {
            st.next_download_id += 1;
            let id = st.next_download_id;
            entry.id = id;
            st.downloads.insert(0, entry);
            id
        });
        self.bridge_state().notify_ui();
        id
    }

    /// Updates status (and optional local file path) of an existing live download entry.
    pub fn update_download_status(
        &self,
        download_id: u64,
        status: DownloadStatus,
        local_path: Option<String>,
    ) {
        with_admin_state(self, |st| {
            if let Some(entry) = st.downloads.iter_mut().find(|d| d.id == download_id) {
                entry.status = status;
                if let Some(p) = local_path {
                    entry.local_path = Some(p);
                }
            }
        });
        self.bridge_state().notify_ui();
    }
}
