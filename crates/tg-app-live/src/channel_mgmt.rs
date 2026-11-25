//! Live channel, supergroup, and basic-group management RPCs for [`LiveSession`] (WP-65).
//!
//! Port origin:
//! - `Telegram/SourceFiles/boxes/peers/edit_peer_type_box.cpp` (`channels.checkUsername`)
//! - `Telegram/SourceFiles/boxes/add_contact_box.cpp` (`SetupChannelBox`: `channels.checkUsername`)
//! - `Telegram/SourceFiles/boxes/peers/edit_peer_info_box.cpp` (`channels.togglePreHistoryHidden`,
//!   `channels.getGroupsForDiscussion`, `channels.deactivateAllUsernames`, `channels.toggleForum`,
//!   `channels.setBoostsToUnblockRestrictions`, `channels.updatePaidMessagesPrice`,
//!   `channels.toggleAutotranslation`)
//! - `Telegram/SourceFiles/boxes/peers/edit_peer_permissions_box.cpp` (`channels.convertToGigagroup`)
//! - `Telegram/SourceFiles/boxes/peers/edit_members_visible.cpp` (`channels.toggleParticipantsHidden`)
//! - `Telegram/SourceFiles/data/data_session.cpp` (`channels.toggleViewForumAsMessages`)
//! - `Telegram/SourceFiles/apiwrap.cpp` (`channels.setStickers`, `channels.setEmojiStickers`)
//! - `Telegram/SourceFiles/data/data_histories.cpp` (`messages.deleteChat`)
//! - `Telegram/SourceFiles/boxes/peers/edit_peer_invite_links.cpp` (`messages.getAdminsWithInvites`)

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use tg_api::RpcClient;
use tg_api::channel_mgmt::{
    ChannelStickerSetInput, apply_deactivate_all_usernames_to_session,
    build_check_channel_username, build_convert_to_gigagroup, build_deactivate_all_usernames,
    build_delete_chat, build_get_admins_with_invites, build_get_groups_for_discussion,
    build_set_boosts_to_unblock_restrictions, build_set_emoji_stickers, build_set_stickers,
    build_toggle_autotranslation, build_toggle_forum, build_toggle_participants_hidden,
    build_toggle_pre_history_hidden, build_toggle_view_forum_as_messages,
    build_update_paid_messages_price,
};
use tg_api::convert::{apply_tl_chat, apply_tl_users_and_chats, input_channel_for, input_peer_for};
use tg_api::rpc::InvocationError;
use tg_data::{
    ChannelFlags, ChannelId, ChatFlags, Peer, PeerId as DataPeerId, Session, UserId as DataUserId,
};
use tg_tl::{RemoteCall, enums as tl_enums, functions as tl_fns};

use crate::bridge::LiveBridgeState;
use crate::convert::{from_data_peer, to_data_peer};
use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::session::LiveSession;
use tg_app_core::info::backend::{
    AdminWithInvitesEntry, ChannelManagementState, ChannelStickerSetRef, DiscussionGroupCandidate,
};
use tg_app_core::info::values::{
    UsernameAvailability, normalize_username, validate_username_syntax,
};
use tg_app_core::settings::backend::{SettingsBackend, UsernameStatus};
use tg_app_core::stand_in::types::PeerId;

/// Default timeout for synchronous channel management RPC invocations.
const CHANNEL_MGMT_RPC_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Default)]
struct ChannelMgmtState {
    sticker_sets: HashMap<ChannelId, ChannelStickerSetRef>,
    emoji_sets: HashMap<ChannelId, ChannelStickerSetRef>,
    common_stars_per_message: HashMap<ChannelId, i64>,
}

struct StateEntry {
    owner: Weak<LiveBridgeState>,
    state: ChannelMgmtState,
}

fn state_registry() -> &'static Mutex<HashMap<usize, StateEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, StateEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_state<R>(live: &LiveSession, f: impl FnOnce(&mut ChannelMgmtState) -> R) -> R {
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    let mut map = state_registry().lock().unwrap_or_else(|e| e.into_inner());
    map.retain(|_, entry| entry.owner.strong_count() > 0);
    let entry = map.entry(key).or_insert_with(|| StateEntry {
        owner: Arc::downgrade(live.bridge_state()),
        state: ChannelMgmtState::default(),
    });
    f(&mut entry.state)
}

fn read_session<R: Send>(live: &LiveSession, f: impl FnOnce(&Session) -> R + Send) -> R {
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

fn write_session<R: Send>(live: &LiveSession, f: impl FnOnce(&mut Session) -> R + Send) -> R {
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
            CHANNEL_MGMT_RPC_TIMEOUT,
            async move { rpc.invoke(&req).await },
        )
        .await
    })
}

fn invoke_rpc_raw<R>(live: &LiveSession, req: R) -> Result<R::Return, InvocationError>
where
    R: RemoteCall + Send + Sync + 'static,
    R::Return: Send + 'static,
{
    let rpc = live.runtime().rpc().clone();
    block_on_runtime(live.runtime().tokio_handle(), async move {
        match tokio::time::timeout(CHANNEL_MGMT_RPC_TIMEOUT, rpc.invoke(&req)).await {
            Ok(Ok(val)) => Ok(val),
            Ok(Err(err)) => Err(err),
            Err(_) => Err(InvocationError::Dropped),
        }
    })
}

fn is_raw_rpc_error(err: &InvocationError, name: &str) -> bool {
    matches!(err, InvocationError::Rpc(rpc_err) if rpc_err.name == name)
}

fn is_chat_not_modified_raw(err: &InvocationError) -> bool {
    is_raw_rpc_error(err, "CHAT_NOT_MODIFIED")
}

/// Feeds an RPC `Updates` reply through `SessionRuntime::apply_updates` (the session actor's
/// single `UpdatesHandler`) and notifies the UI.
fn apply_updates_via_actor(live: &LiveSession, updates: tl_enums::Updates) {
    let runtime = Arc::clone(live.runtime());
    let _ = block_on_runtime(live.runtime().tokio_handle(), async move {
        runtime.apply_updates(updates).await
    });
    live.bridge_state().notify_ui();
}

fn resolve_channel(
    live: &LiveSession,
    peer_id: PeerId,
) -> Result<(ChannelId, tl_enums::InputChannel), String> {
    let data_peer = to_data_peer(peer_id);
    let Some(cid) = data_peer.to_channel() else {
        return Err("Peer is not a channel or supergroup.".to_owned());
    };
    let input = read_session(live, |s| s.channel(cid).map(|_| input_channel_for(s, cid)))
        .ok_or_else(|| "Channel not found in session.".to_owned())?;
    Ok((cid, input))
}

fn sticker_set_ref_to_api(set: &ChannelStickerSetRef) -> ChannelStickerSetInput {
    match set {
        ChannelStickerSetRef::Empty => ChannelStickerSetInput::Empty,
        ChannelStickerSetRef::Id { id, access_hash } => ChannelStickerSetInput::Id {
            id: *id,
            access_hash: *access_hash,
        },
        ChannelStickerSetRef::ShortName(name) => ChannelStickerSetInput::ShortName(name.clone()),
    }
}

// ---------------------------------------------------------------------------
// Cached management state snapshot
// ---------------------------------------------------------------------------

/// Reads cached channel/supergroup management flags from `Session` plus per-session
/// sticker/emoji pack assignments.
pub(crate) fn channel_management_state(
    live: &LiveSession,
    peer_id: PeerId,
) -> Option<ChannelManagementState> {
    let data_peer = to_data_peer(peer_id);
    let cid = data_peer.to_channel()?;
    let mut state = read_session(live, |s| {
        let ch = s.channel(cid)?;
        let (boosts_unrestrict, boosts_applied) = ch.mg_info.as_ref().map_or((0, 0), |mg| {
            (
                mg.boosts_unrestrict.max(0) as u32,
                mg.boosts_applied.max(0) as u32,
            )
        });
        Some(ChannelManagementState {
            pre_history_hidden: ch.flags.contains(ChannelFlags::PRE_HISTORY_HIDDEN),
            is_forum: ch.flags.contains(ChannelFlags::FORUM),
            forum_tabs: ch.flags.contains(ChannelFlags::FORUM_TABS),
            view_forum_as_messages: ch.flags.contains(ChannelFlags::VIEW_AS_MESSAGES),
            participants_hidden: ch.flags.contains(ChannelFlags::PARTICIPANTS_HIDDEN),
            is_gigagroup: ch.flags.contains(ChannelFlags::GIGAGROUP),
            boosts_unrestrict,
            boosts_applied,
            stars_per_message: i64::from(ch.stars_per_message.max(0)),
            broadcast_messages_allowed: ch.broadcast_messages_allowed,
            autotranslation: ch.flags.contains(ChannelFlags::AUTO_TRANSLATION),
            can_set_stickers: ch.flags.contains(ChannelFlags::CAN_SET_STICKERS),
            sticker_set: None,
            emoji_set: None,
        })
    })?;
    with_state(live, |st| {
        state.sticker_set = st.sticker_sets.get(&cid).cloned();
        state.emoji_set = st.emoji_sets.get(&cid).cloned();
        if let Some(&common_stars) = st.common_stars_per_message.get(&cid) {
            state.stars_per_message = common_stars;
        }
    });
    Some(state)
}

// ---------------------------------------------------------------------------
// 1. `channels.checkUsername` (fixes `InfoBackend::check_username`)
// ---------------------------------------------------------------------------

/// Checks username availability for `exclude_peer`:
/// - For a user peer (such as the signed-in account): delegates to `account.checkUsername` via
///   [`SettingsBackend::check_username`].
/// - For a channel/supergroup peer: invokes `channels.checkUsername(InputChannel, username)`
///   (`boxes/peers/edit_peer_type_box.cpp:742-783`).
/// - For a basic group (`Chat`) or `None` (creating a new channel in `CreateChannelBox`):
///   uses the migrated channel's `InputChannel` if migrated, else `InputChannel::Empty`, and
///   invokes `channels.checkUsername` (`edit_peer_type_box.cpp:742-746`).
pub(crate) fn check_username(
    live: &LiveSession,
    username: &str,
    exclude_peer: Option<PeerId>,
) -> UsernameAvailability {
    let syntax = validate_username_syntax(username);
    if syntax != UsernameAvailability::Available {
        return syntax;
    }
    let cleaned = normalize_username(username);

    // User peer -> `account.checkUsername` (`SettingsBackend::check_username`).
    if let Some(pid) = exclude_peer
        && to_data_peer(pid).is_user()
    {
        return match SettingsBackend::check_username(live, username) {
            UsernameStatus::Empty => UsernameAvailability::Empty,
            UsernameStatus::Available | UsernameStatus::Current => UsernameAvailability::Available,
            UsernameStatus::Taken => UsernameAvailability::Occupied,
            UsernameStatus::TooShort => UsernameAvailability::TooShort,
            UsernameStatus::TooLong => UsernameAvailability::TooLong,
            UsernameStatus::InvalidChars => UsernameAvailability::InvalidFormat,
        };
    }

    // Channel, basic group (migrated or unmigrated), or new channel (`None`) ->
    // `channels.checkUsername` (`edit_peer_type_box.cpp:742-783`).
    let (input_channel, current_editable) = match exclude_peer {
        Some(pid) => {
            let data_peer = to_data_peer(pid);
            read_session(live, |s| match s.peer(data_peer) {
                Some(Peer::Channel(ch)) => (
                    input_channel_for(s, ch.channel_id()),
                    ch.usernames.editable_username().to_owned(),
                ),
                Some(Peer::Chat(chat)) => {
                    if let Some(target_cid) = chat.migrated_to {
                        let editable = s
                            .channel(target_cid)
                            .map(|ch| ch.usernames.editable_username().to_owned())
                            .unwrap_or_default();
                        (input_channel_for(s, target_cid), editable)
                    } else {
                        (tl_enums::InputChannel::Empty, String::new())
                    }
                }
                _ => {
                    if let Some(cid) = data_peer.to_channel() {
                        (input_channel_for(s, cid), String::new())
                    } else {
                        (tl_enums::InputChannel::Empty, String::new())
                    }
                }
            })
        }
        None => (tl_enums::InputChannel::Empty, String::new()),
    };

    if !current_editable.is_empty() && cleaned.eq_ignore_ascii_case(&current_editable) {
        return UsernameAvailability::Available;
    }

    let req = build_check_channel_username(input_channel, cleaned);
    match invoke_rpc_raw(live, req) {
        Ok(true) => UsernameAvailability::Available,
        Ok(false) => UsernameAvailability::Occupied,
        Err(InvocationError::Rpc(rpc_err)) => match rpc_err.name.as_str() {
            "CHANNEL_PUBLIC_GROUP_NA" => UsernameAvailability::PublicGroupNotAvailable,
            "CHANNELS_ADMIN_PUBLIC_TOO_MUCH" => UsernameAvailability::TooManyPublicChannels,
            "USERNAME_INVALID" => UsernameAvailability::InvalidFormat,
            "USERNAME_PURCHASE_AVAILABLE" => UsernameAvailability::PurchaseAvailable,
            "USERNAME_OCCUPIED" => UsernameAvailability::Occupied,
            _ => UsernameAvailability::Occupied,
        },
        Err(_) => UsernameAvailability::Occupied,
    }
}

// ---------------------------------------------------------------------------
// 2. `channels.togglePreHistoryHidden`
// ---------------------------------------------------------------------------

/// Toggles pre-history visibility (`channels.togglePreHistoryHidden`,
/// `boxes/peers/edit_peer_info_box.cpp:2817-2847`).
pub(crate) fn toggle_pre_history_hidden(
    live: &mut LiveSession,
    peer_id: PeerId,
    hidden: bool,
) -> Result<(), String> {
    let (cid, channel) = resolve_channel(live, peer_id)?;
    let req = build_toggle_pre_history_hidden(channel, hidden);
    match invoke_rpc_raw(live, req) {
        Ok(updates) => apply_updates_via_actor(live, updates),
        Err(err) if is_chat_not_modified_raw(&err) => {}
        Err(err) => return Err(err.to_string()),
    }
    // `Updates` does not carry `channelFull.hidden_prehistory` (`edit_peer_info_box.cpp:2823-2828`).
    write_session(live, |s| {
        if let Some(Peer::Channel(ch)) = s.peer_mut(DataPeerId::from_channel(cid)) {
            ch.flags.set(ChannelFlags::PRE_HISTORY_HIDDEN, hidden);
        }
    });
    live.bridge_state().notify_ui();
    Ok(())
}

// ---------------------------------------------------------------------------
// 3. `channels.getGroupsForDiscussion`
// ---------------------------------------------------------------------------

/// Fetches groups eligible as a broadcast channel's linked discussion group
/// (`channels.getGroupsForDiscussion`, `boxes/peers/edit_peer_info_box.cpp:1044-1064`).
pub(crate) fn groups_for_discussion(
    live: &LiveSession,
) -> Result<Vec<DiscussionGroupCandidate>, String> {
    let resp = invoke_rpc(live, build_get_groups_for_discussion())?;
    let chats = match resp {
        tl_enums::messages::Chats::Chats(c) => c.chats,
        tl_enums::messages::Chats::Slice(s) => s.chats,
    };
    let candidates = write_session(live, |s| {
        let mut out = Vec::with_capacity(chats.len());
        for chat in &chats {
            let Some(pid) = apply_tl_chat(s, chat) else {
                continue;
            };
            let entry = match s.peer(pid) {
                Some(Peer::Channel(ch)) => DiscussionGroupCandidate {
                    peer_id: from_data_peer(pid),
                    title: ch.common.name.clone(),
                    username: (!ch.usernames.username().is_empty())
                        .then(|| ch.usernames.username().to_owned()),
                    members_count: ch.members_count.max(0) as u32,
                    is_megagroup: ch.is_megagroup(),
                    is_public: ch.is_public(),
                },
                Some(Peer::Chat(c)) => DiscussionGroupCandidate {
                    peer_id: from_data_peer(pid),
                    title: c.common.name.clone(),
                    username: None,
                    members_count: c.count.max(0) as u32,
                    is_megagroup: false,
                    is_public: false,
                },
                _ => continue,
            };
            out.push(entry);
        }
        out
    });
    live.bridge_state().notify_ui();
    Ok(candidates)
}

// ---------------------------------------------------------------------------
// 4. `channels.convertToGigagroup`
// ---------------------------------------------------------------------------

/// Converts a supergroup to a broadcast-style gigagroup
/// (`channels.convertToGigagroup`, `boxes/peers/edit_peer_permissions_box.cpp:1364-1375`).
pub(crate) fn convert_to_gigagroup(live: &mut LiveSession, peer_id: PeerId) -> Result<(), String> {
    let (cid, channel) = resolve_channel(live, peer_id)?;
    let req = build_convert_to_gigagroup(channel);
    match invoke_rpc_raw(live, req) {
        Ok(updates) => apply_updates_via_actor(live, updates),
        Err(err) if is_chat_not_modified_raw(&err) => {}
        Err(err) => return Err(err.to_string()),
    }
    write_session(live, |s| {
        if let Some(Peer::Channel(ch)) = s.peer_mut(DataPeerId::from_channel(cid)) {
            ch.flags.insert(ChannelFlags::GIGAGROUP);
        }
    });
    live.bridge_state().notify_ui();
    Ok(())
}

// ---------------------------------------------------------------------------
// 5. `channels.deactivateAllUsernames`
// ---------------------------------------------------------------------------

/// Deactivates all collectible usernames on a channel/supergroup
/// (`channels.deactivateAllUsernames`, `boxes/peers/edit_peer_info_box.cpp:2490-2501`).
pub(crate) fn deactivate_all_channel_usernames(
    live: &mut LiveSession,
    peer_id: PeerId,
) -> Result<(), String> {
    let (cid, channel) = resolve_channel(live, peer_id)?;
    let ok = invoke_rpc(live, build_deactivate_all_usernames(channel))?;
    if !ok {
        return Err("Server declined deactivating channel usernames.".to_owned());
    }
    write_session(live, |s| {
        apply_deactivate_all_usernames_to_session(s, cid);
    });
    live.bridge_state().notify_ui();
    Ok(())
}

// ---------------------------------------------------------------------------
// 6. `channels.toggleForum`
// ---------------------------------------------------------------------------

/// Toggles forum mode and tabbed subsection layout on a supergroup
/// (`channels.toggleForum`, `boxes/peers/edit_peer_info_box.cpp:2872-2891`).
pub(crate) fn toggle_channel_forum(
    live: &mut LiveSession,
    peer_id: PeerId,
    enabled: bool,
    tabs: bool,
) -> Result<(), String> {
    let (cid, channel) = resolve_channel(live, peer_id)?;
    let effective_tabs = enabled && tabs;
    let req = build_toggle_forum(channel, enabled, effective_tabs, false);
    match invoke_rpc_raw(live, req) {
        Ok(updates) => apply_updates_via_actor(live, updates),
        Err(err) if is_chat_not_modified_raw(&err) => {}
        Err(err) => return Err(err.to_string()),
    }
    write_session(live, |s| {
        if let Some(Peer::Channel(ch)) = s.peer_mut(DataPeerId::from_channel(cid)) {
            let is_megagroup = ch.is_megagroup();
            ch.flags.set(ChannelFlags::FORUM, enabled && is_megagroup);
            ch.flags.set(ChannelFlags::FORUM_TABS, effective_tabs);
        }
    });
    live.bridge_state().notify_ui();
    Ok(())
}

// ---------------------------------------------------------------------------
// 7. `channels.toggleParticipantsHidden`
// ---------------------------------------------------------------------------

/// Hides or reveals the participant list in a supergroup
/// (`channels.toggleParticipantsHidden`, `boxes/peers/edit_members_visible.cpp:71-77`).
pub(crate) fn toggle_participants_hidden(
    live: &mut LiveSession,
    peer_id: PeerId,
    hidden: bool,
) -> Result<(), String> {
    let (cid, channel) = resolve_channel(live, peer_id)?;
    let req = build_toggle_participants_hidden(channel, hidden);
    match invoke_rpc_raw(live, req) {
        Ok(updates) => apply_updates_via_actor(live, updates),
        Err(err) if is_chat_not_modified_raw(&err) => {}
        Err(err) => return Err(err.to_string()),
    }
    write_session(live, |s| {
        if let Some(Peer::Channel(ch)) = s.peer_mut(DataPeerId::from_channel(cid)) {
            ch.flags.set(ChannelFlags::PARTICIPANTS_HIDDEN, hidden);
        }
    });
    live.bridge_state().notify_ui();
    Ok(())
}

// ---------------------------------------------------------------------------
// 8. `channels.toggleViewForumAsMessages`
// ---------------------------------------------------------------------------

/// Toggles viewing a forum supergroup as flat messages vs topic list
/// (`channels.toggleViewForumAsMessages`, `data/data_session.cpp:6014-6027`).
pub(crate) fn toggle_view_forum_as_messages(
    live: &mut LiveSession,
    peer_id: PeerId,
    as_messages: bool,
) -> Result<(), String> {
    let (cid, channel) = resolve_channel(live, peer_id)?;
    let req = build_toggle_view_forum_as_messages(channel, as_messages);
    match invoke_rpc_raw(live, req) {
        Ok(updates) => apply_updates_via_actor(live, updates),
        Err(err) if is_chat_not_modified_raw(&err) => {}
        Err(err) => return Err(err.to_string()),
    }
    write_session(live, |s| {
        if let Some(Peer::Channel(ch)) = s.peer_mut(DataPeerId::from_channel(cid)) {
            ch.flags.set(ChannelFlags::VIEW_AS_MESSAGES, as_messages);
        }
    });
    live.bridge_state().notify_ui();
    Ok(())
}

// ---------------------------------------------------------------------------
// 9. `channels.setBoostsToUnblockRestrictions`
// ---------------------------------------------------------------------------

/// Sets the boost threshold to bypass default group restrictions
/// (`channels.setBoostsToUnblockRestrictions`, `boxes/peers/edit_peer_info_box.cpp:357-386`).
pub(crate) fn set_boosts_to_unblock_restrictions(
    live: &mut LiveSession,
    peer_id: PeerId,
    boosts: u32,
) -> Result<(), String> {
    let (cid, channel) = resolve_channel(live, peer_id)?;
    let boosts_i32 = boosts.min(i32::MAX as u32) as i32;
    let req = build_set_boosts_to_unblock_restrictions(channel, boosts_i32);
    match invoke_rpc_raw(live, req) {
        Ok(updates) => apply_updates_via_actor(live, updates),
        Err(err) if is_chat_not_modified_raw(&err) => {}
        Err(err) => return Err(err.to_string()),
    }
    write_session(live, |s| {
        if let Some(Peer::Channel(ch)) = s.peer_mut(DataPeerId::from_channel(cid)) {
            ch.ensure_mg_info().boosts_unrestrict = boosts_i32;
        }
    });
    live.bridge_state().notify_ui();
    Ok(())
}

// ---------------------------------------------------------------------------
// 10 & 11. `channels.setStickers` & `channels.setEmojiStickers`
// ---------------------------------------------------------------------------

/// Assigns or clears (`ChannelStickerSetRef::Empty`) the official sticker pack of a supergroup
/// (`channels.setStickers`, `apiwrap.cpp:3158-3169`).
pub(crate) fn set_channel_sticker_set(
    live: &mut LiveSession,
    peer_id: PeerId,
    set: ChannelStickerSetRef,
) -> Result<(), String> {
    let (cid, channel) = resolve_channel(live, peer_id)?;
    let api_input = sticker_set_ref_to_api(&set);
    let ok = invoke_rpc(live, build_set_stickers(channel, &api_input))?;
    if !ok {
        return Err("Server declined updating channel sticker pack.".to_owned());
    }
    with_state(live, |st| {
        if matches!(set, ChannelStickerSetRef::Empty) {
            st.sticker_sets.remove(&cid);
        } else {
            st.sticker_sets.insert(cid, set);
        }
    });
    live.bridge_state().notify_ui();
    Ok(())
}

/// Assigns or clears (`ChannelStickerSetRef::Empty`) the official custom emoji pack of a supergroup
/// (`channels.setEmojiStickers`, `apiwrap.cpp:3171-3185`).
pub(crate) fn set_channel_emoji_sticker_set(
    live: &mut LiveSession,
    peer_id: PeerId,
    set: ChannelStickerSetRef,
) -> Result<(), String> {
    let (cid, channel) = resolve_channel(live, peer_id)?;
    let api_input = sticker_set_ref_to_api(&set);
    let ok = invoke_rpc(live, build_set_emoji_stickers(channel, &api_input))?;
    if !ok {
        return Err("Server declined updating channel emoji pack.".to_owned());
    }
    with_state(live, |st| {
        if matches!(set, ChannelStickerSetRef::Empty) {
            st.emoji_sets.remove(&cid);
        } else {
            st.emoji_sets.insert(cid, set);
        }
    });
    live.bridge_state().notify_ui();
    Ok(())
}

// ---------------------------------------------------------------------------
// 12. `channels.updatePaidMessagesPrice`
// ---------------------------------------------------------------------------

/// Updates paid-message Stars pricing and broadcast direct-messages toggle
/// (`channels.updatePaidMessagesPrice`, `boxes/peers/edit_peer_info_box.cpp:314-355`).
pub(crate) fn update_channel_paid_messages_price(
    live: &mut LiveSession,
    peer_id: PeerId,
    broadcast_messages_allowed: bool,
    stars_per_message: i64,
) -> Result<(), String> {
    let (cid, channel) = resolve_channel(live, peer_id)?;
    let is_broadcast = read_session(live, |s| {
        s.channel(cid)
            .is_some_and(|ch| ch.flags.contains(ChannelFlags::BROADCAST))
    });
    let effective_broadcast_allowed =
        is_broadcast && broadcast_messages_allowed && stars_per_message >= 0;
    let clamped_stars = stars_per_message.max(0);
    let req = build_update_paid_messages_price(channel, effective_broadcast_allowed, clamped_stars);
    match invoke_rpc_raw(live, req) {
        Ok(updates) => apply_updates_via_actor(live, updates),
        Err(err) if is_chat_not_modified_raw(&err) => {}
        Err(err) => return Err(err.to_string()),
    }
    let stars_i32 = clamped_stars.min(i64::from(i32::MAX)) as i32;
    write_session(live, |s| {
        if let Some(Peer::Channel(ch)) = s.peer_mut(DataPeerId::from_channel(cid)) {
            ch.flags.insert(ChannelFlags::STARS_PER_MESSAGE_KNOWN);
            if is_broadcast {
                ch.broadcast_messages_allowed = effective_broadcast_allowed;
                let effective_stars = if effective_broadcast_allowed {
                    stars_i32
                } else {
                    0
                };
                ch.stars_per_message = effective_stars;
                ch.flags
                    .set(ChannelFlags::HAS_STARS_PER_MESSAGE, effective_stars > 0);
            } else {
                ch.stars_per_message = stars_i32;
                ch.flags
                    .set(ChannelFlags::HAS_STARS_PER_MESSAGE, stars_i32 > 0);
            }
        }
    });
    with_state(live, |st| {
        if is_broadcast && !effective_broadcast_allowed {
            st.common_stars_per_message.remove(&cid);
        } else {
            st.common_stars_per_message.insert(cid, clamped_stars);
        }
    });
    live.bridge_state().notify_ui();
    Ok(())
}

// ---------------------------------------------------------------------------
// 13. `channels.toggleAutotranslation`
// ---------------------------------------------------------------------------

/// Enables or disables channel post auto-translation
/// (`channels.toggleAutotranslation`, `boxes/peers/edit_peer_info_box.cpp:2893-2914`).
pub(crate) fn toggle_channel_autotranslation(
    live: &mut LiveSession,
    peer_id: PeerId,
    enabled: bool,
) -> Result<(), String> {
    let (cid, channel) = resolve_channel(live, peer_id)?;
    let req = build_toggle_autotranslation(channel, enabled);
    match invoke_rpc_raw(live, req) {
        Ok(updates) => apply_updates_via_actor(live, updates),
        Err(err) if is_chat_not_modified_raw(&err) => {}
        Err(err) => return Err(err.to_string()),
    }
    write_session(live, |s| {
        if let Some(Peer::Channel(ch)) = s.peer_mut(DataPeerId::from_channel(cid)) {
            ch.flags.set(ChannelFlags::AUTO_TRANSLATION, enabled);
        }
    });
    live.bridge_state().notify_ui();
    Ok(())
}

// ---------------------------------------------------------------------------
// 14. `messages.deleteChat`
// ---------------------------------------------------------------------------

/// Permanently deletes a basic group created by the signed-in account
/// (`messages.deleteChat`, `data/data_histories.cpp:875-897`).
///
/// Mirrors tdesktop's `PEER_ID_INVALID` recovery: if the creator previously left the group,
/// rejoins via `messages.addChatUser(self, fwd_limit = 0)`, feeds the returned `Updates`
/// through `SessionRuntime::apply_updates`, and retries `messages.deleteChat` once.
pub(crate) fn delete_basic_chat(live: &mut LiveSession, peer_id: PeerId) -> Result<(), String> {
    let data_peer = to_data_peer(peer_id);
    let Some(chat_id) = data_peer.to_chat() else {
        return Err("Peer is not a basic group.".to_owned());
    };
    match invoke_rpc_raw(live, build_delete_chat(chat_id)) {
        Ok(_) => {}
        Err(err) if is_raw_rpc_error(&err, "PEER_ID_INVALID") => {
            let join_req = tl_fns::messages::AddChatUser {
                chat_id: chat_id.to_tl(),
                user_id: tl_enums::InputUser::Self_,
                fwd_limit: 0,
            };
            let tl_enums::messages::InvitedUsers::InvitedUsers(invited) =
                invoke_rpc(live, join_req)?;
            apply_updates_via_actor(live, invited.updates);
            let _ = invoke_rpc(live, build_delete_chat(chat_id))?;
        }
        Err(err) => return Err(err.to_string()),
    }
    write_session(live, |s| {
        if let Some(Peer::Chat(chat)) = s.peer_mut(data_peer) {
            chat.flags.insert(ChatFlags::DEACTIVATED | ChatFlags::LEFT);
        }
    });
    live.bridge_state().notify_ui();
    Ok(())
}

// ---------------------------------------------------------------------------
// 15. `messages.getAdminsWithInvites`
// ---------------------------------------------------------------------------

/// Lists administrators who have created invite links for `peer_id`
/// (`messages.getAdminsWithInvites`, `boxes/peers/edit_peer_invite_links.cpp:804-822`).
pub(crate) fn admins_with_invites(
    live: &LiveSession,
    peer_id: PeerId,
) -> Result<Vec<AdminWithInvitesEntry>, String> {
    let data_peer = to_data_peer(peer_id);
    let input_peer = read_session(live, |s| input_peer_for(s, data_peer));
    if matches!(input_peer, tl_enums::InputPeer::Empty) {
        return Err("Peer not found in session.".to_owned());
    }
    let tl_enums::messages::ChatAdminsWithInvites::ChatAdminsWithInvites(resp) =
        invoke_rpc(live, build_get_admins_with_invites(input_peer))?;
    let entries = write_session(live, |s| {
        apply_tl_users_and_chats(s, &resp.users, &[]);
        let self_uid = s.self_user_id();
        resp.admins
            .iter()
            .map(|entry| {
                let tl_enums::ChatAdminWithInvites::ChatAdminWithInvites(a) = entry;
                let uid = DataUserId::from_tl(a.admin_id);
                let user_peer = DataPeerId::from_user(uid);
                let (name, username) = match s.user(uid) {
                    Some(u) => {
                        let uname = u.usernames.username();
                        (
                            if u.common.name.is_empty() {
                                format!("User #{}", a.admin_id)
                            } else {
                                u.common.name.clone()
                            },
                            (!uname.is_empty()).then(|| uname.to_owned()),
                        )
                    }
                    None => (format!("User #{}", a.admin_id), None),
                };
                AdminWithInvitesEntry {
                    admin_id: from_data_peer(user_peer),
                    name,
                    username,
                    is_self: uid == self_uid,
                    invites_count: a.invites_count.max(0) as u32,
                    revoked_invites_count: a.revoked_invites_count.max(0) as u32,
                }
            })
            .collect()
    });
    live.bridge_state().notify_ui();
    Ok(entries)
}
