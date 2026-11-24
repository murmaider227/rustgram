//! Channel, supergroup, and basic-group management protocol helpers (WP-65).
//!
//! Port origin:
//! - `boxes/peers/edit_peer_type_box.cpp` (`channels.checkUsername`, `messages.getAdminsWithInvites`)
//! - `boxes/add_contact_box.cpp` (`SetupChannelBox`: `channels.checkUsername`)
//! - `boxes/peers/edit_peer_info_box.cpp` (`channels.togglePreHistoryHidden`,
//!   `channels.getGroupsForDiscussion`, `channels.deactivateAllUsernames`, `channels.toggleForum`,
//!   `channels.setBoostsToUnblockRestrictions`, `channels.updatePaidMessagesPrice`,
//!   `channels.toggleAutotranslation`)
//! - `boxes/peers/edit_peer_permissions_box.cpp` (`channels.convertToGigagroup`)
//! - `boxes/peers/edit_members_visible.cpp` (`channels.toggleParticipantsHidden`)
//! - `data/data_session.cpp` (`channels.toggleViewForumAsMessages`)
//! - `apiwrap.cpp` (`channels.setStickers`, `channels.setEmojiStickers`)
//! - `data/stickers/data_stickers_set.cpp` (`Data::InputStickerSet`)
//! - `data/data_histories.cpp` (`messages.deleteChat` + `PEER_ID_INVALID` self-join retry)
//! - `boxes/peers/edit_peer_invite_links.cpp` (`messages.getAdminsWithInvites`)

use tg_data::{ChannelFlags, ChannelId, ChatId, Peer, PeerId, Session, UserId, Username};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::account::MIN_USERNAME_LENGTH;
use crate::convert::{apply_tl_chat, apply_tl_users_and_chats, input_channel_for, input_peer_for};
use crate::error::ApiError;
use crate::rpc::{InvocationError, RpcClient};

#[inline]
async fn invoke_api<R: RpcClient, F: tg_tl::RemoteCall + Send + Sync>(
    rpc: &R,
    req: &F,
) -> Result<F::Return, ApiError> {
    rpc.invoke(req)
        .await
        .map_err(|err| ApiError::Invocation(err.into()))
}

/// Returns `true` if `err` is an MTProto RPC error with the exact `name` (e.g. `"CHAT_NOT_MODIFIED"`).
#[must_use]
pub fn is_rpc_error_name(err: &ApiError, name: &str) -> bool {
    matches!(
        err,
        ApiError::Invocation(InvocationError::Rpc(rpc_err)) if rpc_err.name == name
    )
}

/// Returns `true` if `err` is `400 CHAT_NOT_MODIFIED`.
#[must_use]
pub fn is_chat_not_modified(err: &ApiError) -> bool {
    is_rpc_error_name(err, "CHAT_NOT_MODIFIED")
}

// ---------------------------------------------------------------------------
// 1. `channels.checkUsername`
// ---------------------------------------------------------------------------

/// Result of checking a channel/supergroup username via `channels.checkUsername`
/// (`boxes/peers/edit_peer_type_box.cpp:742-783`, `boxes/add_contact_box.cpp:1396-1413`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelUsernameCheckResult {
    /// Username is available to claim (or matches the channel's current editable username).
    Available,
    /// Username is already occupied (`false` or `USERNAME_OCCUPIED`).
    Occupied,
    /// Username failed local or server syntax validation (`USERNAME_INVALID`).
    Invalid,
    /// Username is available for purchase on Fragment (`USERNAME_PURCHASE_AVAILABLE`).
    PurchaseAvailable,
    /// Account has reached the public channels/groups limit (`CHANNELS_ADMIN_PUBLIC_TOO_MUCH`).
    TooManyPublicChannels,
    /// Public groups/channels are unavailable for this peer (`CHANNEL_PUBLIC_GROUP_NA`).
    PublicGroupNotAvailable,
}

/// Builds `channels.checkUsername` (`edit_peer_type_box.cpp:744-747`).
#[must_use]
pub fn build_check_channel_username(
    channel: tl_enums::InputChannel,
    username: &str,
) -> tl_fns::channels::CheckUsername {
    tl_fns::channels::CheckUsername {
        channel,
        username: username.trim().trim_start_matches('@').to_owned(),
    }
}

/// Invokes `channels.checkUsername` for an existing channel (`Some(channel_id)`) or a
/// not-yet-created / unmigrated group (`None` -> `InputChannel::Empty`).
///
/// Matches `boxes/peers/edit_peer_type_box.cpp:742-783`:
/// - Returns `Available` when the server returns `true` or when `username` matches the channel's
///   current editable username (`checking == channel->editableUsername()`).
/// - Maps `USERNAME_OCCUPIED`, `USERNAME_INVALID`, `USERNAME_PURCHASE_AVAILABLE`,
///   `CHANNELS_ADMIN_PUBLIC_TOO_MUCH`, and `CHANNEL_PUBLIC_GROUP_NA` cleanly.
pub async fn check_channel_username<R: RpcClient>(
    rpc: &R,
    session: &Session,
    channel_id: Option<ChannelId>,
    username: &str,
) -> Result<ChannelUsernameCheckResult, ApiError> {
    let cleaned = username.trim().trim_start_matches('@');
    if cleaned.len() < MIN_USERNAME_LENGTH
        || !cleaned
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Ok(ChannelUsernameCheckResult::Invalid);
    }

    let (input_channel, current_editable) = match channel_id {
        Some(cid) => {
            let existing = session
                .channel(cid)
                .map(|ch| ch.usernames.editable_username().to_owned())
                .unwrap_or_default();
            (input_channel_for(session, cid), existing)
        }
        None => (tl_enums::InputChannel::Empty, String::new()),
    };

    let req = build_check_channel_username(input_channel, cleaned);
    match invoke_api(rpc, &req).await {
        Ok(true) => Ok(ChannelUsernameCheckResult::Available),
        Ok(false) => {
            if !current_editable.is_empty() && cleaned.eq_ignore_ascii_case(&current_editable) {
                Ok(ChannelUsernameCheckResult::Available)
            } else {
                Ok(ChannelUsernameCheckResult::Occupied)
            }
        }
        Err(err) => {
            if let ApiError::Invocation(InvocationError::Rpc(ref rpc_err)) = err {
                match rpc_err.name.as_str() {
                    "CHANNEL_PUBLIC_GROUP_NA" => {
                        return Ok(ChannelUsernameCheckResult::PublicGroupNotAvailable);
                    }
                    "CHANNELS_ADMIN_PUBLIC_TOO_MUCH" => {
                        return Ok(ChannelUsernameCheckResult::TooManyPublicChannels);
                    }
                    "USERNAME_INVALID" => return Ok(ChannelUsernameCheckResult::Invalid),
                    "USERNAME_PURCHASE_AVAILABLE" => {
                        return Ok(ChannelUsernameCheckResult::PurchaseAvailable);
                    }
                    "USERNAME_OCCUPIED" => {
                        if !current_editable.is_empty()
                            && cleaned.eq_ignore_ascii_case(&current_editable)
                        {
                            return Ok(ChannelUsernameCheckResult::Available);
                        }
                        return Ok(ChannelUsernameCheckResult::Occupied);
                    }
                    _ => {}
                }
            }
            Err(err)
        }
    }
}

// ---------------------------------------------------------------------------
// 2. `channels.togglePreHistoryHidden`
// ---------------------------------------------------------------------------

/// Builds `channels.togglePreHistoryHidden` (`boxes/peers/edit_peer_info_box.cpp:2832-2835`).
#[must_use]
pub fn build_toggle_pre_history_hidden(
    channel: tl_enums::InputChannel,
    enabled: bool,
) -> tl_fns::channels::TogglePreHistoryHidden {
    tl_fns::channels::TogglePreHistoryHidden { channel, enabled }
}

/// Invokes `channels.togglePreHistoryHidden`.
///
/// Matches `Controller::togglePreHistoryHidden` (`edit_peer_info_box.cpp:2817-2847`):
/// treats `CHAT_NOT_MODIFIED` as `Ok(None)` so callers can still apply `PRE_HISTORY_HIDDEN`
/// locally (the returned `Updates` do not carry `channelFull.hidden_prehistory`).
pub async fn toggle_pre_history_hidden<R: RpcClient>(
    rpc: &R,
    session: &Session,
    channel_id: ChannelId,
    enabled: bool,
) -> Result<Option<tl_enums::Updates>, ApiError> {
    let req = build_toggle_pre_history_hidden(input_channel_for(session, channel_id), enabled);
    match invoke_api(rpc, &req).await {
        Ok(updates) => Ok(Some(updates)),
        Err(err) if is_chat_not_modified(&err) => Ok(None),
        Err(err) => Err(err),
    }
}

// ---------------------------------------------------------------------------
// 3. `channels.getGroupsForDiscussion`
// ---------------------------------------------------------------------------

/// Builds `channels.getGroupsForDiscussion` (`boxes/peers/edit_peer_info_box.cpp:1044-1046`).
#[must_use]
pub fn build_get_groups_for_discussion() -> tl_fns::channels::GetGroupsForDiscussion {
    tl_fns::channels::GetGroupsForDiscussion {}
}

/// Invokes `channels.getGroupsForDiscussion`, ingests all returned `chats` into `session`,
/// and returns their [`PeerId`]s in server order (`edit_peer_info_box.cpp:1044-1064`).
pub async fn get_groups_for_discussion<R: RpcClient>(
    rpc: &R,
    session: &mut Session,
) -> Result<Vec<PeerId>, ApiError> {
    let req = build_get_groups_for_discussion();
    let resp = invoke_api(rpc, &req).await?;
    let chats = match resp {
        tl_enums::messages::Chats::Chats(c) => c.chats,
        tl_enums::messages::Chats::Slice(s) => s.chats,
    };
    let mut peers = Vec::with_capacity(chats.len());
    for chat in &chats {
        if let Some(pid) = apply_tl_chat(session, chat) {
            peers.push(pid);
        }
    }
    Ok(peers)
}

// ---------------------------------------------------------------------------
// 4. `channels.convertToGigagroup`
// ---------------------------------------------------------------------------

/// Builds `channels.convertToGigagroup` (`boxes/peers/edit_peer_permissions_box.cpp:1364-1366`).
#[must_use]
pub fn build_convert_to_gigagroup(
    channel: tl_enums::InputChannel,
) -> tl_fns::channels::ConvertToGigagroup {
    tl_fns::channels::ConvertToGigagroup { channel }
}

/// Invokes `channels.convertToGigagroup`, treating `CHAT_NOT_MODIFIED` as `Ok(None)`.
pub async fn convert_to_gigagroup<R: RpcClient>(
    rpc: &R,
    session: &Session,
    channel_id: ChannelId,
) -> Result<Option<tl_enums::Updates>, ApiError> {
    let req = build_convert_to_gigagroup(input_channel_for(session, channel_id));
    match invoke_api(rpc, &req).await {
        Ok(updates) => Ok(Some(updates)),
        Err(err) if is_chat_not_modified(&err) => Ok(None),
        Err(err) => Err(err),
    }
}

// ---------------------------------------------------------------------------
// 5. `channels.deactivateAllUsernames`
// ---------------------------------------------------------------------------

/// Builds `channels.deactivateAllUsernames`
/// (`boxes/peers/edit_peer_info_box.cpp:2490-2492`, `boxes/premium_limits_box.cpp:372-374`).
#[must_use]
pub fn build_deactivate_all_usernames(
    channel: tl_enums::InputChannel,
) -> tl_fns::channels::DeactivateAllUsernames {
    tl_fns::channels::DeactivateAllUsernames { channel }
}

/// Applies the local username-list update after `channels.deactivateAllUsernames` succeeds,
/// matching `edit_peer_info_box.cpp:2493-2499`: preserves only the editable username if set,
/// or clears all active usernames (and `ChannelFlags::USERNAME`) if editable username is empty.
pub fn apply_deactivate_all_usernames_to_session(session: &mut Session, channel_id: ChannelId) {
    let peer_id = PeerId::from_channel(channel_id);
    if let Some(Peer::Channel(ch)) = session.peer_mut(peer_id) {
        let editable = ch.usernames.editable_username().to_owned();
        if editable.is_empty() {
            ch.set_usernames(&[]);
        } else {
            ch.set_usernames(&[Username {
                username: editable,
                active: true,
                editable: true,
            }]);
        }
    }
}

/// Invokes `channels.deactivateAllUsernames` and updates cached usernames in `session`
/// matching `edit_peer_info_box.cpp:2490-2501`.
pub async fn deactivate_all_usernames<R: RpcClient>(
    rpc: &R,
    session: &mut Session,
    channel_id: ChannelId,
) -> Result<bool, ApiError> {
    let req = build_deactivate_all_usernames(input_channel_for(session, channel_id));
    let ok = invoke_api(rpc, &req).await?;
    if ok {
        apply_deactivate_all_usernames_to_session(session, channel_id);
    }
    Ok(ok)
}

// ---------------------------------------------------------------------------
// 6. `channels.toggleForum`
// ---------------------------------------------------------------------------

/// Builds `channels.toggleForum` (`boxes/peers/edit_peer_info_box.cpp:2872-2877`).
///
/// Matches tdesktop's invariant: when `enabled == false`, both `tabs` and `sidebar` are forced
/// to `false`.
#[must_use]
pub fn build_toggle_forum(
    channel: tl_enums::InputChannel,
    enabled: bool,
    tabs: bool,
    sidebar: bool,
) -> tl_fns::channels::ToggleForum {
    tl_fns::channels::ToggleForum {
        channel,
        enabled,
        tabs: enabled && tabs,
        sidebar: enabled && sidebar,
    }
}

/// Invokes `channels.toggleForum`, treating `CHAT_NOT_MODIFIED` as `Ok(None)`
/// (`edit_peer_info_box.cpp:2872-2891`).
pub async fn toggle_forum<R: RpcClient>(
    rpc: &R,
    session: &Session,
    channel_id: ChannelId,
    enabled: bool,
    tabs: bool,
    sidebar: bool,
) -> Result<Option<tl_enums::Updates>, ApiError> {
    let req = build_toggle_forum(
        input_channel_for(session, channel_id),
        enabled,
        tabs,
        sidebar,
    );
    match invoke_api(rpc, &req).await {
        Ok(updates) => Ok(Some(updates)),
        Err(err) if is_chat_not_modified(&err) => Ok(None),
        Err(err) => Err(err),
    }
}

// ---------------------------------------------------------------------------
// 7. `channels.toggleParticipantsHidden`
// ---------------------------------------------------------------------------

/// Builds `channels.toggleParticipantsHidden` (`boxes/peers/edit_members_visible.cpp:71-74`).
#[must_use]
pub fn build_toggle_participants_hidden(
    channel: tl_enums::InputChannel,
    enabled: bool,
) -> tl_fns::channels::ToggleParticipantsHidden {
    tl_fns::channels::ToggleParticipantsHidden { channel, enabled }
}

/// Invokes `channels.toggleParticipantsHidden`, treating `CHAT_NOT_MODIFIED` as `Ok(None)`.
pub async fn toggle_participants_hidden<R: RpcClient>(
    rpc: &R,
    session: &Session,
    channel_id: ChannelId,
    enabled: bool,
) -> Result<Option<tl_enums::Updates>, ApiError> {
    let req = build_toggle_participants_hidden(input_channel_for(session, channel_id), enabled);
    match invoke_api(rpc, &req).await {
        Ok(updates) => Ok(Some(updates)),
        Err(err) if is_chat_not_modified(&err) => Ok(None),
        Err(err) => Err(err),
    }
}

// ---------------------------------------------------------------------------
// 8. `channels.toggleViewForumAsMessages`
// ---------------------------------------------------------------------------

/// Builds `channels.toggleViewForumAsMessages` (`data/data_session.cpp:6018-6020`).
#[must_use]
pub fn build_toggle_view_forum_as_messages(
    channel: tl_enums::InputChannel,
    enabled: bool,
) -> tl_fns::channels::ToggleViewForumAsMessages {
    tl_fns::channels::ToggleViewForumAsMessages { channel, enabled }
}

/// Invokes `channels.toggleViewForumAsMessages`, treating `CHAT_NOT_MODIFIED` as `Ok(None)`.
pub async fn toggle_view_forum_as_messages<R: RpcClient>(
    rpc: &R,
    session: &Session,
    channel_id: ChannelId,
    enabled: bool,
) -> Result<Option<tl_enums::Updates>, ApiError> {
    let req = build_toggle_view_forum_as_messages(input_channel_for(session, channel_id), enabled);
    match invoke_api(rpc, &req).await {
        Ok(updates) => Ok(Some(updates)),
        Err(err) if is_chat_not_modified(&err) => Ok(None),
        Err(err) => Err(err),
    }
}

// ---------------------------------------------------------------------------
// 9. `channels.setBoostsToUnblockRestrictions`
// ---------------------------------------------------------------------------

/// Builds `channels.setBoostsToUnblockRestrictions`
/// (`boxes/peers/edit_peer_info_box.cpp:364-366`).
#[must_use]
pub fn build_set_boosts_to_unblock_restrictions(
    channel: tl_enums::InputChannel,
    boosts: i32,
) -> tl_fns::channels::SetBoostsToUnblockRestrictions {
    tl_fns::channels::SetBoostsToUnblockRestrictions {
        channel,
        boosts: boosts.max(0),
    }
}

/// Invokes `channels.setBoostsToUnblockRestrictions`, treating `CHAT_NOT_MODIFIED` as `Ok(None)`
/// (`edit_peer_info_box.cpp:357-386`).
pub async fn set_boosts_to_unblock_restrictions<R: RpcClient>(
    rpc: &R,
    session: &Session,
    channel_id: ChannelId,
    boosts: i32,
) -> Result<Option<tl_enums::Updates>, ApiError> {
    let req =
        build_set_boosts_to_unblock_restrictions(input_channel_for(session, channel_id), boosts);
    match invoke_api(rpc, &req).await {
        Ok(updates) => Ok(Some(updates)),
        Err(err) if is_chat_not_modified(&err) => Ok(None),
        Err(err) => Err(err),
    }
}

// ---------------------------------------------------------------------------
// 10 & 11. `channels.setStickers` & `channels.setEmojiStickers`
// ---------------------------------------------------------------------------

/// Sticker/emoji pack reference for `channels.setStickers` and `channels.setEmojiStickers`
/// (`Data::InputStickerSet` in `data/stickers/data_stickers_set.cpp:228-234`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ChannelStickerSetInput {
    /// Clears the channel's assigned sticker or emoji pack (`inputStickerSetEmpty`).
    #[default]
    Empty,
    /// References a pack by `(id, access_hash)` (`inputStickerSetID`).
    Id {
        /// Server sticker set ID.
        id: i64,
        /// Server sticker set access hash.
        access_hash: i64,
    },
    /// References a pack by short name (`inputStickerSetShortName`).
    ShortName(String),
}

impl ChannelStickerSetInput {
    /// Converts into TL `InputStickerSet` matching `Data::InputStickerSet`
    /// (`data/stickers/data_stickers_set.cpp:228-234`).
    #[must_use]
    pub fn to_tl(&self) -> tl_enums::InputStickerSet {
        match self {
            Self::Empty => tl_enums::InputStickerSet::Empty,
            Self::Id { id, access_hash } if *id != 0 => {
                tl_enums::InputStickerSet::ID(tl_types::InputStickerSetID {
                    id: *id,
                    access_hash: *access_hash,
                })
            }
            Self::Id { .. } => tl_enums::InputStickerSet::Empty,
            Self::ShortName(name) if !name.trim().is_empty() => {
                tl_enums::InputStickerSet::ShortName(tl_types::InputStickerSetShortName {
                    short_name: name.trim().to_owned(),
                })
            }
            Self::ShortName(_) => tl_enums::InputStickerSet::Empty,
        }
    }
}

/// Builds `channels.setStickers` (`apiwrap.cpp:3164-3167`).
#[must_use]
pub fn build_set_stickers(
    channel: tl_enums::InputChannel,
    stickerset: &ChannelStickerSetInput,
) -> tl_fns::channels::SetStickers {
    tl_fns::channels::SetStickers {
        channel,
        stickerset: stickerset.to_tl(),
    }
}

/// Invokes `channels.setStickers` (`apiwrap.cpp:3158-3169`).
pub async fn set_stickers<R: RpcClient>(
    rpc: &R,
    session: &Session,
    channel_id: ChannelId,
    stickerset: &ChannelStickerSetInput,
) -> Result<bool, ApiError> {
    let req = build_set_stickers(input_channel_for(session, channel_id), stickerset);
    invoke_api(rpc, &req).await
}

/// Builds `channels.setEmojiStickers` (`apiwrap.cpp:3177-3180`).
#[must_use]
pub fn build_set_emoji_stickers(
    channel: tl_enums::InputChannel,
    stickerset: &ChannelStickerSetInput,
) -> tl_fns::channels::SetEmojiStickers {
    tl_fns::channels::SetEmojiStickers {
        channel,
        stickerset: stickerset.to_tl(),
    }
}

/// Invokes `channels.setEmojiStickers` (`apiwrap.cpp:3171-3185`).
pub async fn set_emoji_stickers<R: RpcClient>(
    rpc: &R,
    session: &Session,
    channel_id: ChannelId,
    stickerset: &ChannelStickerSetInput,
) -> Result<bool, ApiError> {
    let req = build_set_emoji_stickers(input_channel_for(session, channel_id), stickerset);
    invoke_api(rpc, &req).await
}

// ---------------------------------------------------------------------------
// 12. `channels.updatePaidMessagesPrice`
// ---------------------------------------------------------------------------

/// Builds `channels.updatePaidMessagesPrice` (`boxes/peers/edit_peer_info_box.cpp:324-332`).
///
/// - For a broadcast channel, `broadcast_messages_allowed = true` enables direct messages
///   (with `send_paid_messages_stars >= 0`, where `0` is free), while `false` disables them.
/// - For a megagroup/supergroup, `broadcast_messages_allowed` must be `false`.
#[must_use]
pub fn build_update_paid_messages_price(
    channel: tl_enums::InputChannel,
    broadcast_messages_allowed: bool,
    send_paid_messages_stars: i64,
) -> tl_fns::channels::UpdatePaidMessagesPrice {
    tl_fns::channels::UpdatePaidMessagesPrice {
        broadcast_messages_allowed,
        channel,
        send_paid_messages_stars: send_paid_messages_stars.max(0),
    }
}

/// Invokes `channels.updatePaidMessagesPrice` (`edit_peer_info_box.cpp:314-355`).
///
/// Enforces tdesktop's `broadcast && (stars >= 0)` rule: if the cached channel is not a broadcast
/// channel, `broadcast_messages_allowed` is forced to `false`. Treats `CHAT_NOT_MODIFIED` as
/// `Ok(None)`.
pub async fn update_paid_messages_price<R: RpcClient>(
    rpc: &R,
    session: &Session,
    channel_id: ChannelId,
    broadcast_messages_allowed: bool,
    send_paid_messages_stars: i64,
) -> Result<Option<tl_enums::Updates>, ApiError> {
    let is_broadcast = session
        .channel(channel_id)
        .is_some_and(|ch| ch.flags.contains(ChannelFlags::BROADCAST));
    let effective_broadcast_allowed =
        is_broadcast && broadcast_messages_allowed && send_paid_messages_stars >= 0;
    let req = build_update_paid_messages_price(
        input_channel_for(session, channel_id),
        effective_broadcast_allowed,
        send_paid_messages_stars,
    );
    match invoke_api(rpc, &req).await {
        Ok(updates) => Ok(Some(updates)),
        Err(err) if is_chat_not_modified(&err) => Ok(None),
        Err(err) => Err(err),
    }
}

// ---------------------------------------------------------------------------
// 13. `channels.toggleAutotranslation`
// ---------------------------------------------------------------------------

/// Builds `channels.toggleAutotranslation` (`boxes/peers/edit_peer_info_box.cpp:2900-2903`).
#[must_use]
pub fn build_toggle_autotranslation(
    channel: tl_enums::InputChannel,
    enabled: bool,
) -> tl_fns::channels::ToggleAutotranslation {
    tl_fns::channels::ToggleAutotranslation { channel, enabled }
}

/// Invokes `channels.toggleAutotranslation`, treating `CHAT_NOT_MODIFIED` as `Ok(None)`
/// (`edit_peer_info_box.cpp:2893-2914`).
pub async fn toggle_autotranslation<R: RpcClient>(
    rpc: &R,
    session: &Session,
    channel_id: ChannelId,
    enabled: bool,
) -> Result<Option<tl_enums::Updates>, ApiError> {
    let req = build_toggle_autotranslation(input_channel_for(session, channel_id), enabled);
    match invoke_api(rpc, &req).await {
        Ok(updates) => Ok(Some(updates)),
        Err(err) if is_chat_not_modified(&err) => Ok(None),
        Err(err) => Err(err),
    }
}

// ---------------------------------------------------------------------------
// 14. `messages.deleteChat`
// ---------------------------------------------------------------------------

/// Outcome of `messages.deleteChat` (`data/data_histories.cpp:876-897`).
#[derive(Clone, Debug, PartialEq)]
pub enum DeleteChatOutcome {
    /// Basic group was deleted directly on the first `messages.deleteChat` attempt.
    Deleted,
    /// First attempt returned `PEER_ID_INVALID` (creator had left); rejoined via
    /// `messages.addChatUser(self, fwd_limit=0)` and deleted on retry.
    JoinedAndDeleted {
        /// Updates returned by `messages.addChatUser`, to be applied by the caller's session actor.
        join_updates: Box<tl_enums::Updates>,
    },
}

/// Builds `messages.deleteChat` (`data/data_histories.cpp:876-878`).
#[must_use]
pub fn build_delete_chat(chat_id: ChatId) -> tl_fns::messages::DeleteChat {
    tl_fns::messages::DeleteChat {
        chat_id: chat_id.to_tl(),
    }
}

/// Invokes `messages.deleteChat` for a basic group (`data/data_histories.cpp:875-897`).
///
/// Matches tdesktop's exact `PEER_ID_INVALID` recovery: when a creator tries to delete a basic
/// group they previously left, Telegram replies `PEER_ID_INVALID`; tdesktop calls
/// `messages.addChatUser(chat_id, InputUser::Self_, fwd_limit: 0)` and retries
/// `messages.deleteChat` once.
pub async fn delete_chat<R: RpcClient>(
    rpc: &R,
    chat_id: ChatId,
) -> Result<DeleteChatOutcome, ApiError> {
    let req = build_delete_chat(chat_id);
    match invoke_api(rpc, &req).await {
        Ok(_) => Ok(DeleteChatOutcome::Deleted),
        Err(err) if is_rpc_error_name(&err, "PEER_ID_INVALID") => {
            let join_req = tl_fns::messages::AddChatUser {
                chat_id: chat_id.to_tl(),
                user_id: tl_enums::InputUser::Self_,
                fwd_limit: 0,
            };
            let tl_enums::messages::InvitedUsers::InvitedUsers(invited) =
                invoke_api(rpc, &join_req).await?;
            let _ = invoke_api(rpc, &build_delete_chat(chat_id)).await?;
            Ok(DeleteChatOutcome::JoinedAndDeleted {
                join_updates: Box::new(invited.updates),
            })
        }
        Err(err) => Err(err),
    }
}

// ---------------------------------------------------------------------------
// 15. `messages.getAdminsWithInvites`
// ---------------------------------------------------------------------------

/// Parsed `ChatAdminWithInvites` record (`boxes/peers/edit_peer_invite_links.cpp:804-822`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChatAdminWithInvitesInfo {
    /// Administrator user ID (`admin_id`).
    pub admin_id: UserId,
    /// Number of active invite links created by this admin (`invites_count`).
    pub invites_count: i32,
    /// Number of revoked invite links created by this admin (`revoked_invites_count`).
    pub revoked_invites_count: i32,
}

/// Builds `messages.getAdminsWithInvites`
/// (`boxes/peers/edit_peer_invite_links.cpp:804-806`, `edit_peer_type_box.cpp:326-328`).
#[must_use]
pub fn build_get_admins_with_invites(
    peer: tl_enums::InputPeer,
) -> tl_fns::messages::GetAdminsWithInvites {
    tl_fns::messages::GetAdminsWithInvites { peer }
}

/// Invokes `messages.getAdminsWithInvites`, ingests `resp.users` into `session`, and returns all
/// [`ChatAdminWithInvitesInfo`] entries in server order.
pub async fn get_admins_with_invites<R: RpcClient>(
    rpc: &R,
    session: &mut Session,
    peer_id: PeerId,
) -> Result<Vec<ChatAdminWithInvitesInfo>, ApiError> {
    let req = build_get_admins_with_invites(input_peer_for(session, peer_id));
    let tl_enums::messages::ChatAdminsWithInvites::ChatAdminsWithInvites(resp) =
        invoke_api(rpc, &req).await?;
    apply_tl_users_and_chats(session, &resp.users, &[]);
    Ok(resp
        .admins
        .iter()
        .map(|entry| {
            let tl_enums::ChatAdminWithInvites::ChatAdminWithInvites(a) = entry;
            ChatAdminWithInvitesInfo {
                admin_id: UserId::from_tl(a.admin_id),
                invites_count: a.invites_count,
                revoked_invites_count: a.revoked_invites_count,
            }
        })
        .collect())
}
