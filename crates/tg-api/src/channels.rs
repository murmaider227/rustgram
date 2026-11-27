//! Channels, supergroups, admin actions, invite links, join requests, admin log,
//! read contents, and recommendations protocol helpers.
//!
//! Port origin:
//! - `api/api_chat_participants.cpp` (`ChatParticipants`, `getParticipants`, `getParticipant`,
//!   `getChannelRecommendations`)
//! - `api/api_invite_links.cpp` (`InviteLinks`, `exportChatInvite`, `editExportedChatInvite`,
//!   `getExportedChatInvites`, `getChatInviteImporters`)
//! - `api/api_hash.h` (`HashInit`, `HashUpdate`, `HashFinalize`, `CountHash`)
//! - `data/data_channel.cpp` (`KickedRestrictedRights`)
//! - `data/data_chat_participant_status.cpp` (`AdminRightsToMTP`, `RestrictionsToMTP`)
//! - `boxes/peers/edit_participants_box.cpp` (`editAdmin`, `editBanned`)
//! - `boxes/peers/edit_peer_info_box.cpp` (`toggleSlowMode`, `toggleSignatures`,
//!   `toggleJoinToSend`, `toggleJoinRequest`)
//! - `boxes/peers/edit_peer_requests_box.cpp` (`hideChatJoinRequest`, `hideAllChatJoinRequests`)
//! - `history/admin_log/history_admin_log_inner.cpp` (`channels.getAdminLog` paging & filter)
//! - `apiwrap.cpp` (`channels.readMessageContents` / `messages.readMessageContents`)

use std::ops::{BitOr, BitOrAssign};

use tg_data::{
    ChannelId, ChatAdminRights, ChatBannedRights, ChatRestrictionsInfo, MsgId, PeerId, Session,
    TimeId, UserId,
};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::convert::{
    admin_rights_from_tl, admin_rights_to_tl, apply_tl_users_and_chats, banned_rights_from_tl,
    banned_rights_to_tl, input_channel_for, input_peer_for, input_user_for, peer_id_from_tl,
};
use crate::error::ApiError;
use crate::rpc::RpcClient;

#[inline]
async fn invoke_api<R: RpcClient, F: tg_tl::RemoteCall + Send + Sync>(
    rpc: &R,
    req: &F,
) -> Result<F::Return, ApiError> {
    rpc.invoke(req)
        .await
        .map_err(|err| ApiError::Invocation(err.into()))
}

/// Default page size for `channels.getAdminLog` first load (`kEventsFirstPage` in
/// `history/admin_log/history_admin_log_inner.cpp:97`).
pub const ADMIN_LOG_FIRST_PAGE: i32 = 100;

/// Default page size for `channels.getAdminLog` subsequent loads (`kEventsPerPage` in
/// `history/admin_log/history_admin_log_inner.cpp:98`).
pub const ADMIN_LOG_PER_PAGE: i32 = 100;

/// First page limit for `messages.getExportedChatInvites` (`kFirstPage` in `api_invite_links.cpp`).
pub const EXPORTED_INVITES_FIRST_PAGE: i32 = 10;

/// Subsequent page limit for `messages.getExportedChatInvites` (`kPerPage` in `api_invite_links.cpp`).
pub const EXPORTED_INVITES_PER_PAGE: i32 = 50;

/// First page limit for `messages.getChatInviteImporters` (`kFirstPage` in `api_invite_links.cpp`).
pub const INVITE_IMPORTERS_FIRST_PAGE: i32 = 10;

/// Subsequent page limit for `messages.getChatInviteImporters` (`kPerPage` in `api_invite_links.cpp`).
pub const INVITE_IMPORTERS_PER_PAGE: i32 = 50;

/// Default page size for `channels.getParticipants` (`kPerPage` in `api_chat_participants.cpp`).
pub const PARTICIPANTS_PER_PAGE: i32 = 200;

// ---------------------------------------------------------------------------
// Telegram 64-bit pagination/cache hash (`api/api_hash.h:36-70`)
// ---------------------------------------------------------------------------

/// Initializes a Telegram 64-bit list hash (`Api::HashInit` in `api/api_hash.h:36`).
#[must_use]
#[inline]
pub const fn api_hash_init() -> u64 {
    0
}

/// Folds a `u64` value into a Telegram 64-bit list hash (`Api::HashUpdate` in `api/api_hash.h:40-45`).
#[inline]
pub const fn api_hash_update_u64(hash: &mut u64, value: u64) {
    *hash ^= *hash >> 21;
    *hash ^= *hash << 35;
    *hash ^= *hash >> 4;
    *hash = hash.wrapping_add(value);
}

/// Folds an `i64` value into a Telegram 64-bit list hash (`Api::HashUpdate(uint64&, int64)`).
#[inline]
pub const fn api_hash_update_i64(hash: &mut u64, value: i64) {
    api_hash_update_u64(hash, value as u64);
}

/// Folds a `u32` value into a Telegram 64-bit list hash (`Api::HashUpdate(uint64&, uint32)`).
#[inline]
pub const fn api_hash_update_u32(hash: &mut u64, value: u32) {
    api_hash_update_u64(hash, value as u64);
}

/// Folds an `i32` value into a Telegram 64-bit list hash (`Api::HashUpdate(uint64&, int32)`).
///
/// Sign-extends to `i64` first, matching `api/api_hash.h:55-57`.
#[inline]
pub const fn api_hash_update_i32(hash: &mut u64, value: i32) {
    api_hash_update_i64(hash, value as i64);
}

/// Finalizes a Telegram 64-bit list hash as signed TL `long` (`Api::HashFinalize`).
#[must_use]
#[inline]
pub const fn api_hash_finalize(hash: u64) -> i64 {
    hash as i64
}

/// Computes `Api::CountHash` over an iterator of `i64` values (`api/api_hash.h:64-70`).
#[must_use]
pub fn count_hash_i64<I: IntoIterator<Item = i64>>(values: I) -> i64 {
    let mut hash = api_hash_init();
    for v in values {
        api_hash_update_i64(&mut hash, v);
    }
    api_hash_finalize(hash)
}

/// Computes `Api::CountHash` over an iterator of `i32` values (`api/api_hash.h:64-70`).
#[must_use]
pub fn count_hash_i32<I: IntoIterator<Item = i32>>(values: I) -> i64 {
    let mut hash = api_hash_init();
    for v in values {
        api_hash_update_i32(&mut hash, v);
    }
    api_hash_finalize(hash)
}

// ---------------------------------------------------------------------------
// Participants (`channels.getParticipants`, `channels.getParticipant`)
// ---------------------------------------------------------------------------

/// Role classification of a `ChannelParticipant` (`Api::ChatParticipant::Type`
/// in `api/api_chat_participants.cpp:273-323`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ChannelParticipantKind {
    /// Channel/supergroup creator (`channelParticipantCreator`).
    Creator,
    /// Promoted administrator (`channelParticipantAdmin`).
    Admin,
    /// Regular member (`channelParticipant` or `channelParticipantSelf`).
    Member,
    /// Restricted member still in the group (`channelParticipantBanned` with `left: false`).
    Restricted,
    /// Permanently kicked/banned member (`channelParticipantBanned` with `left: true`).
    Banned,
    /// Peer who left on their own (`channelParticipantLeft`).
    Left,
}

/// Parsed `ChannelParticipant` entry (`Api::ChatParticipant` in `api_chat_participants.cpp`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelParticipantInfo {
    /// Role classification.
    pub kind: ChannelParticipantKind,
    /// Participant peer ID (user, or channel/chat for banned/left non-user peers).
    pub peer_id: PeerId,
    /// Inviter user ID (if known).
    pub inviter_id: Option<UserId>,
    /// Admin who promoted or banned this peer (if known).
    pub by_user_id: Option<UserId>,
    /// Join, promotion, or ban timestamp (`0` if omitted).
    pub date: TimeId,
    /// Whether this participant is the logged-in user (`channelParticipantSelf` or `admin.self`).
    pub is_self: bool,
    /// Whether the user joined via join request (`channelParticipantSelf.via_request`).
    pub via_request: bool,
    /// Whether the current user can edit this admin's rights (`channelParticipantAdmin.can_edit`).
    pub can_edit: bool,
    /// Admin rights (non-empty for `Creator` and `Admin`).
    pub admin_rights: ChatAdminRights,
    /// Restrictions (populated for `Restricted` and `Banned`).
    pub restrictions: ChatRestrictionsInfo,
    /// Custom title / member rank (`rank`).
    pub rank: Option<String>,
    /// Paid subscription expiry date (`subscription_until_date`), if any.
    pub subscription_until_date: Option<TimeId>,
}

/// Converts a TL `ChannelParticipant` into [`ChannelParticipantInfo`]
/// (`Api::ChatParticipant::ChatParticipant` in `api/api_chat_participants.cpp:273-323`).
#[must_use]
pub fn channel_participant_from_tl(p: &tl_enums::ChannelParticipant) -> ChannelParticipantInfo {
    match p {
        tl_enums::ChannelParticipant::ChannelParticipant(m) => ChannelParticipantInfo {
            kind: ChannelParticipantKind::Member,
            peer_id: PeerId::from_user(UserId::from_tl(m.user_id)),
            inviter_id: None,
            by_user_id: None,
            date: m.date,
            is_self: false,
            via_request: false,
            can_edit: false,
            admin_rights: ChatAdminRights::empty(),
            restrictions: ChatRestrictionsInfo::default(),
            rank: m.rank.clone(),
            subscription_until_date: m.subscription_until_date,
        },
        tl_enums::ChannelParticipant::Self_(m) => ChannelParticipantInfo {
            kind: ChannelParticipantKind::Member,
            peer_id: PeerId::from_user(UserId::from_tl(m.user_id)),
            inviter_id: Some(UserId::from_tl(m.inviter_id)),
            by_user_id: None,
            date: m.date,
            is_self: true,
            via_request: m.via_request,
            can_edit: false,
            admin_rights: ChatAdminRights::empty(),
            restrictions: ChatRestrictionsInfo::default(),
            rank: m.rank.clone(),
            subscription_until_date: m.subscription_until_date,
        },
        tl_enums::ChannelParticipant::Creator(m) => ChannelParticipantInfo {
            kind: ChannelParticipantKind::Creator,
            peer_id: PeerId::from_user(UserId::from_tl(m.user_id)),
            inviter_id: None,
            by_user_id: None,
            date: 0,
            is_self: false,
            via_request: false,
            can_edit: false,
            admin_rights: admin_rights_from_tl(&m.admin_rights),
            restrictions: ChatRestrictionsInfo::default(),
            rank: m.rank.clone(),
            subscription_until_date: None,
        },
        tl_enums::ChannelParticipant::Admin(m) => ChannelParticipantInfo {
            kind: ChannelParticipantKind::Admin,
            peer_id: PeerId::from_user(UserId::from_tl(m.user_id)),
            inviter_id: m.inviter_id.map(UserId::from_tl),
            by_user_id: Some(UserId::from_tl(m.promoted_by)),
            date: m.date,
            is_self: m.self_,
            via_request: false,
            can_edit: m.can_edit,
            admin_rights: admin_rights_from_tl(&m.admin_rights),
            restrictions: ChatRestrictionsInfo::default(),
            rank: m.rank.clone(),
            subscription_until_date: None,
        },
        tl_enums::ChannelParticipant::Banned(m) => {
            let restrictions = banned_rights_from_tl(&m.banned_rights);
            let kind = if m.left || restrictions.flags.contains(ChatBannedRights::VIEW_MESSAGES) {
                ChannelParticipantKind::Banned
            } else {
                ChannelParticipantKind::Restricted
            };
            ChannelParticipantInfo {
                kind,
                peer_id: peer_id_from_tl(&m.peer),
                inviter_id: None,
                by_user_id: Some(UserId::from_tl(m.kicked_by)),
                date: m.date,
                is_self: false,
                via_request: false,
                can_edit: false,
                admin_rights: ChatAdminRights::empty(),
                restrictions,
                rank: m.rank.clone(),
                subscription_until_date: None,
            }
        }
        tl_enums::ChannelParticipant::Left(m) => ChannelParticipantInfo {
            kind: ChannelParticipantKind::Left,
            peer_id: peer_id_from_tl(&m.peer),
            inviter_id: None,
            by_user_id: None,
            date: 0,
            is_self: false,
            via_request: false,
            can_edit: false,
            admin_rights: ChatAdminRights::empty(),
            restrictions: ChatRestrictionsInfo::default(),
            rank: None,
            subscription_until_date: None,
        },
    }
}

/// Filter for `channels.getParticipants` covering all Layer 233 `ChannelParticipantsFilter`
/// constructors (`api/api_chat_participants.cpp`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChannelParticipantsFilter {
    /// `channelParticipantsRecent`.
    Recent,
    /// `channelParticipantsAdmins`.
    Admins,
    /// `channelParticipantsKicked` (permanently banned / kicked members).
    Kicked {
        /// Optional search query.
        query: String,
    },
    /// `channelParticipantsBots`.
    Bots,
    /// `channelParticipantsBanned` (restricted members still in group).
    Banned {
        /// Optional search query.
        query: String,
    },
    /// `channelParticipantsSearch`.
    Search {
        /// Search query.
        query: String,
    },
    /// `channelParticipantsContacts`.
    Contacts {
        /// Optional search query.
        query: String,
    },
    /// `channelParticipantsMentions`.
    Mentions {
        /// Optional search query.
        query: Option<String>,
        /// Optional forum topic / thread top message ID.
        top_msg_id: Option<MsgId>,
    },
}

impl ChannelParticipantsFilter {
    /// Converts this filter into the corresponding TL `ChannelParticipantsFilter`.
    #[must_use]
    pub fn to_tl(&self) -> tl_enums::ChannelParticipantsFilter {
        match self {
            Self::Recent => tl_enums::ChannelParticipantsFilter::ChannelParticipantsRecent,
            Self::Admins => tl_enums::ChannelParticipantsFilter::ChannelParticipantsAdmins,
            Self::Kicked { query } => {
                tl_enums::ChannelParticipantsFilter::ChannelParticipantsKicked(
                    tl_types::ChannelParticipantsKicked { q: query.clone() },
                )
            }
            Self::Bots => tl_enums::ChannelParticipantsFilter::ChannelParticipantsBots,
            Self::Banned { query } => {
                tl_enums::ChannelParticipantsFilter::ChannelParticipantsBanned(
                    tl_types::ChannelParticipantsBanned { q: query.clone() },
                )
            }
            Self::Search { query } => {
                tl_enums::ChannelParticipantsFilter::ChannelParticipantsSearch(
                    tl_types::ChannelParticipantsSearch { q: query.clone() },
                )
            }
            Self::Contacts { query } => {
                tl_enums::ChannelParticipantsFilter::ChannelParticipantsContacts(
                    tl_types::ChannelParticipantsContacts { q: query.clone() },
                )
            }
            Self::Mentions { query, top_msg_id } => {
                tl_enums::ChannelParticipantsFilter::ChannelParticipantsMentions(
                    tl_types::ChannelParticipantsMentions {
                        q: query.clone().filter(|s| !s.is_empty()),
                        top_msg_id: top_msg_id.and_then(MsgId::server_i32),
                    },
                )
            }
        }
    }
}

/// Parsed response of `channels.getParticipants`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChannelParticipantsPage {
    /// `channels.channelParticipantsNotModified`: local list matching `hash` is still current.
    NotModified,
    /// `channels.channelParticipants`: total count and parsed participant entries.
    Participants {
        /// Server-reported total participant count for this filter.
        count: i32,
        /// Parsed participant records in page order.
        participants: Vec<ChannelParticipantInfo>,
    },
}

/// Builds `channels.getParticipants` (`api/api_chat_participants.cpp`).
#[must_use]
pub fn build_get_participants(
    channel: tl_enums::InputChannel,
    filter: &ChannelParticipantsFilter,
    offset: i32,
    limit: i32,
    hash: i64,
) -> tl_fns::channels::GetParticipants {
    tl_fns::channels::GetParticipants {
        channel,
        filter: filter.to_tl(),
        offset,
        limit,
        hash,
    }
}

/// Invokes `channels.getParticipants`, ingests accompanying `users` and `chats` into `session`,
/// and returns [`ChannelParticipantsPage`].
pub async fn get_participants<R: RpcClient>(
    rpc: &R,
    session: &mut Session,
    channel_id: ChannelId,
    filter: &ChannelParticipantsFilter,
    offset: i32,
    limit: i32,
    hash: i64,
) -> Result<ChannelParticipantsPage, ApiError> {
    let req = build_get_participants(
        input_channel_for(session, channel_id),
        filter,
        offset,
        limit,
        hash,
    );
    let resp = invoke_api(rpc, &req).await?;
    match resp {
        tl_enums::channels::ChannelParticipants::NotModified => {
            Ok(ChannelParticipantsPage::NotModified)
        }
        tl_enums::channels::ChannelParticipants::ChannelParticipants(data) => {
            apply_tl_users_and_chats(session, &data.users, &data.chats);
            let participants = data
                .participants
                .iter()
                .map(channel_participant_from_tl)
                .collect();
            Ok(ChannelParticipantsPage::Participants {
                count: data.count,
                participants,
            })
        }
    }
}

/// Builds `channels.getParticipant`.
#[must_use]
pub fn build_get_participant(
    channel: tl_enums::InputChannel,
    participant: tl_enums::InputPeer,
) -> tl_fns::channels::GetParticipant {
    tl_fns::channels::GetParticipant {
        channel,
        participant,
    }
}

/// Invokes `channels.getParticipant`, ingests `users` and `chats` into `session`, and returns
/// the parsed [`ChannelParticipantInfo`].
pub async fn get_participant<R: RpcClient>(
    rpc: &R,
    session: &mut Session,
    channel_id: ChannelId,
    participant_peer_id: PeerId,
) -> Result<ChannelParticipantInfo, ApiError> {
    let req = build_get_participant(
        input_channel_for(session, channel_id),
        input_peer_for(session, participant_peer_id),
    );
    let tl_enums::channels::ChannelParticipant::ChannelParticipant(resp) =
        invoke_api(rpc, &req).await?;
    apply_tl_users_and_chats(session, &resp.users, &resp.chats);
    Ok(channel_participant_from_tl(&resp.participant))
}

// ---------------------------------------------------------------------------
// Admin rights & restrictions (`channels.editAdmin`, `channels.editBanned`)
// ---------------------------------------------------------------------------

/// Builds `channels.editAdmin` (`boxes/peers/edit_participants_box.cpp:71-78, 174-183`).
///
/// Note on `rank`: in Layer 233 `rank:flags.0?string` distinguishes `None` (leave rank unchanged)
/// from `Some("")` (explicitly clear custom title, e.g. when clearing rank or removing an admin).
#[must_use]
pub fn build_edit_admin(
    channel: tl_enums::InputChannel,
    user_id: tl_enums::InputUser,
    admin_rights: ChatAdminRights,
    rank: Option<&str>,
) -> tl_fns::channels::EditAdmin {
    tl_fns::channels::EditAdmin {
        channel,
        user_id,
        admin_rights: admin_rights_to_tl(admin_rights),
        rank: rank.map(str::to_owned),
    }
}

/// Invokes `channels.editAdmin` for `(channel_id, user_id)`.
pub async fn edit_admin<R: RpcClient>(
    rpc: &R,
    session: &Session,
    channel_id: ChannelId,
    user_id: UserId,
    admin_rights: ChatAdminRights,
    rank: Option<&str>,
) -> Result<tl_enums::Updates, ApiError> {
    let req = build_edit_admin(
        input_channel_for(session, channel_id),
        input_user_for(session, user_id),
        admin_rights,
        rank,
    );
    invoke_api(rpc, &req).await
}

/// Returns the `ChatRestrictionsInfo` used to kick/ban `peer_id` from a channel or supergroup
/// (`KickedRestrictedRights` in `data/data_channel.cpp:495-514`).
///
/// - For user peers: `ChatBannedRights::KICKED_USER` (`VIEW_MESSAGES | SEND_MESSAGES | SEND_MEDIA
///   | SEND_STICKERS | SEND_GIFS | SEND_GAMES | SEND_INLINE | EMBED_LINKS`, serialized via
///   `banned_rights_to_tl` without legacy `send_messages`/`send_media` bits) with `until_date = 0`.
/// - For non-user peers (e.g. banned channel posting as itself): `ChatBannedRights::VIEW_MESSAGES`
///   only, with `until_date = 0`.
#[must_use]
pub fn kicked_restrictions_for_peer(peer_id: PeerId) -> ChatRestrictionsInfo {
    let flags = if peer_id.is_user() {
        ChatBannedRights::KICKED_USER
    } else {
        ChatBannedRights::VIEW_MESSAGES
    };
    ChatRestrictionsInfo::new(flags, 0)
}

/// Normalizes a `chatBannedRights.until_date` timestamp per Telegram / tdesktop rules
/// (`data/data_chat_participant_status.cpp`):
/// - `0` or `i32::MAX` (or negative) means forever (`0` on the wire when kicking or restricting forever).
/// - Positive finite timestamps `< i32::MAX` are preserved verbatim.
#[must_use]
pub const fn normalize_ban_until_date(until_date: TimeId) -> TimeId {
    if until_date <= 0 || until_date == i32::MAX {
        0
    } else {
        until_date
    }
}

/// Builds `channels.editBanned` (`api/api_chat_participants.cpp` / `data/data_chat_participant_status.cpp`).
#[must_use]
pub fn build_edit_banned(
    channel: tl_enums::InputChannel,
    participant: tl_enums::InputPeer,
    restrictions: ChatRestrictionsInfo,
) -> tl_fns::channels::EditBanned {
    tl_fns::channels::EditBanned {
        channel,
        participant,
        banned_rights: banned_rights_to_tl(restrictions),
    }
}

/// Invokes `channels.editBanned` for `(channel_id, participant_peer_id)`.
pub async fn edit_banned<R: RpcClient>(
    rpc: &R,
    session: &Session,
    channel_id: ChannelId,
    participant_peer_id: PeerId,
    restrictions: ChatRestrictionsInfo,
) -> Result<tl_enums::Updates, ApiError> {
    let req = build_edit_banned(
        input_channel_for(session, channel_id),
        input_peer_for(session, participant_peer_id),
        restrictions,
    );
    invoke_api(rpc, &req).await
}

// ---------------------------------------------------------------------------
// Admin Log (`channels.getAdminLog`)
// ---------------------------------------------------------------------------

/// Event filter bitflags for `channels.getAdminLog` (`channelAdminLogEventsFilter#ea107ae4`
/// in `schema/api.tl` and `history/admin_log/history_admin_log_filter.h`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct AdminLogEventsFilter(u32);

impl AdminLogEventsFilter {
    /// `join:flags.0?true`
    pub const JOIN: Self = Self(1 << 0);
    /// `leave:flags.1?true`
    pub const LEAVE: Self = Self(1 << 1);
    /// `invite:flags.2?true`
    pub const INVITE: Self = Self(1 << 2);
    /// `ban:flags.3?true`
    pub const BAN: Self = Self(1 << 3);
    /// `unban:flags.4?true`
    pub const UNBAN: Self = Self(1 << 4);
    /// `kick:flags.5?true`
    pub const KICK: Self = Self(1 << 5);
    /// `unkick:flags.6?true`
    pub const UNKICK: Self = Self(1 << 6);
    /// `promote:flags.7?true`
    pub const PROMOTE: Self = Self(1 << 7);
    /// `demote:flags.8?true`
    pub const DEMOTE: Self = Self(1 << 8);
    /// `info:flags.9?true`
    pub const INFO: Self = Self(1 << 9);
    /// `settings:flags.10?true`
    pub const SETTINGS: Self = Self(1 << 10);
    /// `pinned:flags.11?true`
    pub const PINNED: Self = Self(1 << 11);
    /// `edit:flags.12?true`
    pub const EDIT: Self = Self(1 << 12);
    /// `delete:flags.13?true`
    pub const DELETE: Self = Self(1 << 13);
    /// `group_call:flags.14?true`
    pub const GROUP_CALL: Self = Self(1 << 14);
    /// `invites:flags.15?true`
    pub const INVITES: Self = Self(1 << 15);
    /// `send:flags.16?true`
    pub const SEND: Self = Self(1 << 16);
    /// `forums:flags.17?true`
    pub const FORUMS: Self = Self(1 << 17);
    /// `sub_extend:flags.18?true`
    pub const SUB_EXTEND: Self = Self(1 << 18);
    /// `edit_rank:flags.19?true`
    pub const EDIT_RANK: Self = Self(1 << 19);

    /// Empty filter.
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// All 20 Layer 233 event flags set.
    #[must_use]
    pub const fn all() -> Self {
        Self((1 << 20) - 1)
    }

    /// Returns `true` if all bits in `other` are set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Returns raw bitmask.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }
}

impl BitOr for AdminLogEventsFilter {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for AdminLogEventsFilter {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl AdminLogEventsFilter {
    /// Converts these flags into TL `ChannelAdminLogEventsFilter`.
    #[must_use]
    pub fn to_tl(self) -> tl_enums::ChannelAdminLogEventsFilter {
        tl_enums::ChannelAdminLogEventsFilter::ChannelAdminLogEventsFilter(
            tl_types::ChannelAdminLogEventsFilter {
                join: self.contains(Self::JOIN),
                leave: self.contains(Self::LEAVE),
                invite: self.contains(Self::INVITE),
                ban: self.contains(Self::BAN),
                unban: self.contains(Self::UNBAN),
                kick: self.contains(Self::KICK),
                unkick: self.contains(Self::UNKICK),
                promote: self.contains(Self::PROMOTE),
                demote: self.contains(Self::DEMOTE),
                info: self.contains(Self::INFO),
                settings: self.contains(Self::SETTINGS),
                pinned: self.contains(Self::PINNED),
                edit: self.contains(Self::EDIT),
                delete: self.contains(Self::DELETE),
                group_call: self.contains(Self::GROUP_CALL),
                invites: self.contains(Self::INVITES),
                send: self.contains(Self::SEND),
                forums: self.contains(Self::FORUMS),
                sub_extend: self.contains(Self::SUB_EXTEND),
                edit_rank: self.contains(Self::EDIT_RANK),
            },
        )
    }
}

/// Parameters for `channels.getAdminLog` (`history/admin_log/history_admin_log_inner.cpp:1194-1275`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdminLogQuery {
    /// Search query string (`q`).
    pub query: String,
    /// Optional event type filter (`None` means all events; `events_filter` omitted on wire).
    pub events_filter: Option<AdminLogEventsFilter>,
    /// Optional admin user IDs filter (`None` or empty omits `admins` flag on wire).
    pub admins: Vec<UserId>,
    /// Load events strictly older than `max_id` (`0` for newest page / loading newer via `min_id`).
    pub max_id: i64,
    /// Load events strictly newer than `min_id` (`0` for loading older via `max_id`).
    pub min_id: i64,
    /// Maximum number of events to return (`ADMIN_LOG_FIRST_PAGE` / `ADMIN_LOG_PER_PAGE` = 100).
    pub limit: i32,
}

/// Parsed page returned by `channels.getAdminLog`.
#[derive(Clone, Debug, PartialEq)]
pub struct AdminLogPage {
    /// Raw TL events in server order (newest first).
    pub events: Vec<tl_enums::ChannelAdminLogEvent>,
    /// Minimum event ID in this batch (for paging older via `max_id`), if non-empty.
    pub min_event_id: Option<i64>,
    /// Maximum event ID in this batch (for polling newer via `min_id`), if non-empty.
    pub max_event_id: Option<i64>,
}

/// Builds `channels.getAdminLog` (`history/admin_log/history_admin_log_inner.cpp:1203-1226`).
#[must_use]
pub fn build_get_admin_log(
    session: &Session,
    channel_id: ChannelId,
    query: &AdminLogQuery,
) -> tl_fns::channels::GetAdminLog {
    let admins = if query.admins.is_empty() {
        None
    } else {
        Some(
            query
                .admins
                .iter()
                .map(|&uid| input_user_for(session, uid))
                .collect(),
        )
    };
    let limit = if query.limit > 0 {
        query.limit
    } else {
        ADMIN_LOG_PER_PAGE
    };
    tl_fns::channels::GetAdminLog {
        channel: input_channel_for(session, channel_id),
        q: query.query.clone(),
        events_filter: query.events_filter.map(AdminLogEventsFilter::to_tl),
        admins,
        max_id: query.max_id,
        min_id: query.min_id,
        limit,
    }
}

/// Invokes `channels.getAdminLog`, ingests `users` and `chats` into `session`, and returns
/// [`AdminLogPage`] with computed `min_event_id` / `max_event_id` paging cursors.
pub async fn get_admin_log<R: RpcClient>(
    rpc: &R,
    session: &mut Session,
    channel_id: ChannelId,
    query: &AdminLogQuery,
) -> Result<AdminLogPage, ApiError> {
    let req = build_get_admin_log(session, channel_id, query);
    let tl_enums::channels::AdminLogResults::AdminLogResults(resp) = invoke_api(rpc, &req).await?;
    apply_tl_users_and_chats(session, &resp.users, &resp.chats);
    let mut min_id: Option<i64> = None;
    let mut max_id: Option<i64> = None;
    for ev in &resp.events {
        let tl_enums::ChannelAdminLogEvent::ChannelAdminLogEvent(e) = ev;
        min_id = Some(min_id.map_or(e.id, |cur| cur.min(e.id)));
        max_id = Some(max_id.map_or(e.id, |cur| cur.max(e.id)));
    }
    Ok(AdminLogPage {
        events: resp.events,
        min_event_id: min_id,
        max_event_id: max_id,
    })
}

// ---------------------------------------------------------------------------
// Channel toggles (`toggleSlowMode`, `toggleSignatures`, `toggleJoinToSend`, `toggleJoinRequest`)
// ---------------------------------------------------------------------------

/// Builds `channels.toggleSlowMode` (`seconds = 0` disables slow mode).
#[must_use]
pub fn build_toggle_slow_mode(
    channel: tl_enums::InputChannel,
    seconds: i32,
) -> tl_fns::channels::ToggleSlowMode {
    tl_fns::channels::ToggleSlowMode { channel, seconds }
}

/// Invokes `channels.toggleSlowMode`.
pub async fn toggle_slow_mode<R: RpcClient>(
    rpc: &R,
    session: &Session,
    channel_id: ChannelId,
    seconds: i32,
) -> Result<tl_enums::Updates, ApiError> {
    let req = build_toggle_slow_mode(input_channel_for(session, channel_id), seconds);
    invoke_api(rpc, &req).await
}

/// Builds `channels.toggleSignatures` with Layer 233 `signatures_enabled` and `profiles_enabled`
/// flags (`boxes/peers/edit_peer_info_box.cpp:2989-2996`).
///
/// Note: when `signatures_enabled` is `false`, `profiles_enabled` is forced `false` (profile
/// links require signatures to be enabled).
#[must_use]
pub fn build_toggle_signatures(
    channel: tl_enums::InputChannel,
    signatures_enabled: bool,
    profiles_enabled: bool,
) -> tl_fns::channels::ToggleSignatures {
    tl_fns::channels::ToggleSignatures {
        signatures_enabled,
        profiles_enabled: signatures_enabled && profiles_enabled,
        channel,
    }
}

/// Invokes `channels.toggleSignatures`.
pub async fn toggle_signatures<R: RpcClient>(
    rpc: &R,
    session: &Session,
    channel_id: ChannelId,
    signatures_enabled: bool,
    profiles_enabled: bool,
) -> Result<tl_enums::Updates, ApiError> {
    let req = build_toggle_signatures(
        input_channel_for(session, channel_id),
        signatures_enabled,
        profiles_enabled,
    );
    invoke_api(rpc, &req).await
}

/// Builds `channels.toggleJoinToSend`.
#[must_use]
pub fn build_toggle_join_to_send(
    channel: tl_enums::InputChannel,
    enabled: bool,
) -> tl_fns::channels::ToggleJoinToSend {
    tl_fns::channels::ToggleJoinToSend { channel, enabled }
}

/// Invokes `channels.toggleJoinToSend`.
pub async fn toggle_join_to_send<R: RpcClient>(
    rpc: &R,
    session: &Session,
    channel_id: ChannelId,
    enabled: bool,
) -> Result<tl_enums::Updates, ApiError> {
    let req = build_toggle_join_to_send(input_channel_for(session, channel_id), enabled);
    invoke_api(rpc, &req).await
}

/// Builds `channels.toggleJoinRequest` with Layer 233 `apply_to_invites` and `guard_bot` flags.
#[must_use]
pub fn build_toggle_join_request(
    channel: tl_enums::InputChannel,
    enabled: bool,
    apply_to_invites: bool,
    guard_bot: Option<tl_enums::InputUser>,
) -> tl_fns::channels::ToggleJoinRequest {
    tl_fns::channels::ToggleJoinRequest {
        apply_to_invites,
        channel,
        enabled,
        guard_bot,
    }
}

/// Invokes `channels.toggleJoinRequest`.
pub async fn toggle_join_request<R: RpcClient>(
    rpc: &R,
    session: &Session,
    channel_id: ChannelId,
    enabled: bool,
    apply_to_invites: bool,
    guard_bot: Option<UserId>,
) -> Result<tl_enums::Updates, ApiError> {
    let req = build_toggle_join_request(
        input_channel_for(session, channel_id),
        enabled,
        apply_to_invites,
        guard_bot.map(|uid| input_user_for(session, uid)),
    );
    invoke_api(rpc, &req).await
}

// ---------------------------------------------------------------------------
// Join Requests (`messages.hideChatJoinRequest`, `messages.hideAllChatJoinRequests`)
// ---------------------------------------------------------------------------

/// Builds `messages.hideChatJoinRequest` (`approved: true` approves, `false` declines).
#[must_use]
pub fn build_hide_chat_join_request(
    peer: tl_enums::InputPeer,
    user_id: tl_enums::InputUser,
    approved: bool,
) -> tl_fns::messages::HideChatJoinRequest {
    tl_fns::messages::HideChatJoinRequest {
        approved,
        peer,
        user_id,
    }
}

/// Invokes `messages.hideChatJoinRequest`.
pub async fn hide_chat_join_request<R: RpcClient>(
    rpc: &R,
    session: &Session,
    peer_id: PeerId,
    user_id: UserId,
    approved: bool,
) -> Result<tl_enums::Updates, ApiError> {
    let req = build_hide_chat_join_request(
        input_peer_for(session, peer_id),
        input_user_for(session, user_id),
        approved,
    );
    invoke_api(rpc, &req).await
}

/// Builds `messages.hideAllChatJoinRequests` (optionally scoped to a specific invite `link`).
#[must_use]
pub fn build_hide_all_chat_join_requests(
    peer: tl_enums::InputPeer,
    approved: bool,
    link: Option<&str>,
) -> tl_fns::messages::HideAllChatJoinRequests {
    tl_fns::messages::HideAllChatJoinRequests {
        approved,
        peer,
        link: link.filter(|s| !s.is_empty()).map(str::to_owned),
    }
}

/// Invokes `messages.hideAllChatJoinRequests`.
pub async fn hide_all_chat_join_requests<R: RpcClient>(
    rpc: &R,
    session: &Session,
    peer_id: PeerId,
    approved: bool,
    link: Option<&str>,
) -> Result<tl_enums::Updates, ApiError> {
    let req = build_hide_all_chat_join_requests(input_peer_for(session, peer_id), approved, link);
    invoke_api(rpc, &req).await
}

// ---------------------------------------------------------------------------
// Chat Invite Importers (`messages.getChatInviteImporters`)
// ---------------------------------------------------------------------------

/// Parsed `ChatInviteImporter` record (`api/api_invite_links.cpp:479-535`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatInviteImporterInfo {
    /// Importing or requesting user ID.
    pub user_id: UserId,
    /// Import or request timestamp.
    pub date: TimeId,
    /// Whether this entry is a pending join request (`requested: true`).
    pub requested: bool,
    /// Whether the user joined via a folder invite link (`via_chatlist: true`).
    pub via_chatlist: bool,
    /// Optional bio snippet shown on pending join requests.
    pub about: Option<String>,
    /// Admin who approved the join request, if applicable.
    pub approved_by: Option<UserId>,
}

/// Query parameters for `messages.getChatInviteImporters` (`api/api_invite_links.cpp:479-535`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChatInviteImportersQuery {
    /// Request pending join requests (`requested: true`) instead of joined importers.
    pub requested: bool,
    /// Request users with expired subscriptions (`subscription_expired: true`).
    pub subscription_expired: bool,
    /// Specific invite link to query (omitted when `None` or empty).
    pub link: Option<String>,
    /// Optional search query over pending requests (`q`, mutually exclusive with `link` in Telegram).
    pub query: Option<String>,
    /// Pagination cursor date (`0` for first page; last importer's `date` for next page).
    pub offset_date: TimeId,
    /// Pagination cursor user (`None` -> `InputUser::Empty` on first page; last importer's user on next page).
    pub offset_user: Option<UserId>,
    /// Page limit (`0` selects tdesktop defaults: 10 on first page, 50 on subsequent pages).
    pub limit: i32,
}

/// Parsed page returned by `messages.getChatInviteImporters`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatInviteImportersPage {
    /// Server-reported total count.
    pub count: i32,
    /// Importers in this page.
    pub importers: Vec<ChatInviteImporterInfo>,
    /// Cursor `(offset_date, offset_user)` for loading the next page, if any items were returned.
    pub next_offset: Option<(TimeId, UserId)>,
}

/// Builds `messages.getChatInviteImporters` (`api/api_invite_links.cpp:479-535`).
#[must_use]
pub fn build_get_chat_invite_importers(
    session: &Session,
    peer_id: PeerId,
    query: &ChatInviteImportersQuery,
) -> tl_fns::messages::GetChatInviteImporters {
    let is_first_page = query.offset_date == 0 && query.offset_user.is_none();
    let limit = if query.limit > 0 {
        query.limit
    } else if is_first_page {
        INVITE_IMPORTERS_FIRST_PAGE
    } else {
        INVITE_IMPORTERS_PER_PAGE
    };
    let offset_user = query.offset_user.map_or(tl_enums::InputUser::Empty, |uid| {
        input_user_for(session, uid)
    });
    tl_fns::messages::GetChatInviteImporters {
        requested: query.requested,
        subscription_expired: query.subscription_expired,
        peer: input_peer_for(session, peer_id),
        link: query.link.clone().filter(|s| !s.is_empty()),
        q: query.query.clone().filter(|s| !s.is_empty()),
        offset_date: query.offset_date,
        offset_user,
        limit,
    }
}

/// Invokes `messages.getChatInviteImporters`, ingests `users` into `session`, and returns
/// [`ChatInviteImportersPage`].
pub async fn get_chat_invite_importers<R: RpcClient>(
    rpc: &R,
    session: &mut Session,
    peer_id: PeerId,
    query: &ChatInviteImportersQuery,
) -> Result<ChatInviteImportersPage, ApiError> {
    let req = build_get_chat_invite_importers(session, peer_id, query);
    let tl_enums::messages::ChatInviteImporters::ChatInviteImporters(resp) =
        invoke_api(rpc, &req).await?;
    apply_tl_users_and_chats(session, &resp.users, &[]);
    let importers: Vec<ChatInviteImporterInfo> = resp
        .importers
        .iter()
        .map(|imp| {
            let tl_enums::ChatInviteImporter::ChatInviteImporter(d) = imp;
            ChatInviteImporterInfo {
                user_id: UserId::from_tl(d.user_id),
                date: d.date,
                requested: d.requested,
                via_chatlist: d.via_chatlist,
                about: d.about.clone(),
                approved_by: d.approved_by.map(UserId::from_tl),
            }
        })
        .collect();
    let next_offset = importers.last().map(|last| (last.date, last.user_id));
    Ok(ChatInviteImportersPage {
        count: resp.count,
        importers,
        next_offset,
    })
}

// ---------------------------------------------------------------------------
// Exported Chat Invites (`exportChatInvite`, `editExportedChatInvite`, `getExportedChatInvites`)
// ---------------------------------------------------------------------------

/// Parsed `chatInviteExported` record (`Api::InviteLink` in `api/api_invite_links.cpp`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportedChatInviteInfo {
    /// Full invite URL (`https://t.me/+...`).
    pub link: String,
    /// Optional human-readable label (`title`).
    pub title: String,
    /// Creator admin user ID.
    pub admin_id: UserId,
    /// Creation timestamp.
    pub date: TimeId,
    /// Optional start date for statistics.
    pub start_date: TimeId,
    /// Optional expiration timestamp (`0` = never expires).
    pub expire_date: TimeId,
    /// Maximum uses (`0` = unlimited).
    pub usage_limit: i32,
    /// Current join count.
    pub usage: i32,
    /// Pending join requests count on this link.
    pub requested: i32,
    /// Expired subscription count on this link.
    pub subscription_expired: i32,
    /// Whether this is the peer's primary permanent link.
    pub permanent: bool,
    /// Whether this link has been revoked.
    pub revoked: bool,
    /// Whether joining via this link requires admin approval.
    pub request_needed: bool,
}

/// Converts TL `ExportedChatInvite` into `Option<ExportedChatInviteInfo>`.
#[must_use]
pub fn exported_chat_invite_from_tl(
    invite: &tl_enums::ExportedChatInvite,
) -> Option<ExportedChatInviteInfo> {
    match invite {
        tl_enums::ExportedChatInvite::ChatInviteExported(d) => Some(ExportedChatInviteInfo {
            link: d.link.clone(),
            title: d.title.clone().unwrap_or_default(),
            admin_id: UserId::from_tl(d.admin_id),
            date: d.date,
            start_date: d.start_date.unwrap_or(0),
            expire_date: d.expire_date.unwrap_or(0),
            usage_limit: d.usage_limit.unwrap_or(0),
            usage: d.usage.unwrap_or(0),
            requested: d.requested.unwrap_or(0),
            subscription_expired: d.subscription_expired.unwrap_or(0),
            permanent: d.permanent,
            revoked: d.revoked,
            request_needed: d.request_needed,
        }),
        tl_enums::ExportedChatInvite::ChatInvitePublicJoinRequests => None,
    }
}

/// Options for `messages.exportChatInvite` (`Api::InviteLinks::create` in `api_invite_links.cpp:325-355`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExportChatInviteOptions {
    /// Revoke old permanent link (legacy flag).
    pub legacy_revoke_permanent: bool,
    /// Require admin approval for joiners (`request_needed`).
    ///
    /// Per tdesktop (`api_invite_links.cpp:330`), when `request_needed` is `true`, `usage_limit`
    /// is omitted (`None`) because Telegram forbids combining approval-required with a usage limit.
    pub request_needed: bool,
    /// Optional expiration unix timestamp (`Some(ts)` if `ts > 0`).
    pub expire_date: Option<TimeId>,
    /// Optional usage limit (`Some(n)` if `n > 0` and `!request_needed`).
    pub usage_limit: Option<i32>,
    /// Optional invite label (`title`).
    pub title: Option<String>,
    /// Optional Telegram Stars subscription pricing.
    pub subscription_pricing: Option<tl_enums::StarsSubscriptionPricing>,
}

/// Builds `messages.exportChatInvite` (`api/api_invite_links.cpp:325-355`).
#[must_use]
pub fn build_export_chat_invite(
    peer: tl_enums::InputPeer,
    options: &ExportChatInviteOptions,
) -> tl_fns::messages::ExportChatInvite {
    let usage_limit = if options.request_needed {
        None
    } else {
        options.usage_limit.filter(|&n| n > 0)
    };
    tl_fns::messages::ExportChatInvite {
        legacy_revoke_permanent: options.legacy_revoke_permanent,
        request_needed: options.request_needed,
        peer,
        expire_date: options.expire_date.filter(|&ts| ts > 0),
        usage_limit,
        title: options.title.clone().filter(|s| !s.is_empty()),
        subscription_pricing: options.subscription_pricing.clone(),
    }
}

/// Invokes `messages.exportChatInvite` and parses the returned `ExportedChatInvite`.
pub async fn export_chat_invite<R: RpcClient>(
    rpc: &R,
    session: &Session,
    peer_id: PeerId,
    options: &ExportChatInviteOptions,
) -> Result<Option<ExportedChatInviteInfo>, ApiError> {
    let req = build_export_chat_invite(input_peer_for(session, peer_id), options);
    let resp = invoke_api(rpc, &req).await?;
    Ok(exported_chat_invite_from_tl(&resp))
}

/// Options for `messages.editExportedChatInvite` (`Api::InviteLinks::edit` / `revoke`
/// in `api/api_invite_links.cpp:365-430`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditChatInviteOptions {
    /// Revoke the link (`revoked: true`). When `true`, other edit fields are omitted unless set.
    pub revoked: bool,
    /// Optional new expiration timestamp (`Some(0)` clears expiry, `None` leaves unchanged).
    pub expire_date: Option<TimeId>,
    /// Optional new usage limit (`Some(0)` removes limit, `None` leaves unchanged).
    /// Omitted automatically if `request_needed == Some(true)`.
    pub usage_limit: Option<i32>,
    /// Optional new `request_needed` setting (`None` leaves unchanged).
    pub request_needed: Option<bool>,
    /// Optional new label (`Some("")` clears title, `None` leaves unchanged).
    pub title: Option<String>,
}

/// Result of `messages.editExportedChatInvite`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditChatInviteResult {
    /// `messages.exportedChatInvite`: single edited/revoked link.
    Edited(Option<ExportedChatInviteInfo>),
    /// `messages.exportedChatInviteReplaced`: permanent link was revoked and replaced with `new_invite`.
    Replaced {
        /// The old (now revoked) invite link.
        old_invite: Option<ExportedChatInviteInfo>,
        /// The newly generated replacement permanent invite link.
        new_invite: Option<ExportedChatInviteInfo>,
    },
}

/// Builds `messages.editExportedChatInvite` (`api/api_invite_links.cpp:365-430`).
#[must_use]
pub fn build_edit_exported_chat_invite(
    peer: tl_enums::InputPeer,
    link: String,
    options: &EditChatInviteOptions,
) -> tl_fns::messages::EditExportedChatInvite {
    let usage_limit = if options.request_needed == Some(true) {
        None
    } else {
        options.usage_limit
    };
    tl_fns::messages::EditExportedChatInvite {
        revoked: options.revoked,
        peer,
        link,
        expire_date: options.expire_date,
        usage_limit,
        request_needed: options.request_needed,
        title: options.title.clone(),
    }
}

/// Invokes `messages.editExportedChatInvite`, ingests `users` into `session`, and handles both
/// `messages.exportedChatInvite` and `messages.exportedChatInviteReplaced`.
pub async fn edit_exported_chat_invite<R: RpcClient>(
    rpc: &R,
    session: &mut Session,
    peer_id: PeerId,
    link: String,
    options: &EditChatInviteOptions,
) -> Result<EditChatInviteResult, ApiError> {
    let req = build_edit_exported_chat_invite(input_peer_for(session, peer_id), link, options);
    let resp = invoke_api(rpc, &req).await?;
    match resp {
        tl_enums::messages::ExportedChatInvite::ExportedChatInvite(d) => {
            apply_tl_users_and_chats(session, &d.users, &[]);
            Ok(EditChatInviteResult::Edited(exported_chat_invite_from_tl(
                &d.invite,
            )))
        }
        tl_enums::messages::ExportedChatInvite::Replaced(d) => {
            apply_tl_users_and_chats(session, &d.users, &[]);
            Ok(EditChatInviteResult::Replaced {
                old_invite: exported_chat_invite_from_tl(&d.invite),
                new_invite: exported_chat_invite_from_tl(&d.new_invite),
            })
        }
    }
}

/// Query parameters for `messages.getExportedChatInvites` (`api/api_invite_links.cpp:175-245`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportedChatInvitesQuery {
    /// Admin whose links to fetch (`InputUser`).
    pub admin_id: UserId,
    /// Fetch revoked links (`revoked: true`) instead of active links.
    pub revoked: bool,
    /// Pagination cursor `(offset_date, offset_link)`. In Layer 233 both share `flags.2`,
    /// so they are always set or omitted together.
    pub offset: Option<(TimeId, String)>,
    /// Page limit (`0` selects tdesktop defaults: 10 on first page, 50 on subsequent pages).
    pub limit: i32,
}

/// Parsed page returned by `messages.getExportedChatInvites`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportedChatInvitesPage {
    /// Server-reported total count.
    pub count: i32,
    /// Invites returned in this page.
    pub invites: Vec<ExportedChatInviteInfo>,
    /// Next pagination cursor `(offset_date, offset_link)`, if this page was non-empty.
    pub next_offset: Option<(TimeId, String)>,
}

/// Builds `messages.getExportedChatInvites` (`api/api_invite_links.cpp:175-245`).
#[must_use]
pub fn build_get_exported_chat_invites(
    session: &Session,
    peer_id: PeerId,
    query: &ExportedChatInvitesQuery,
) -> tl_fns::messages::GetExportedChatInvites {
    let is_first_page = query.offset.is_none();
    let limit = if query.limit > 0 {
        query.limit
    } else if is_first_page {
        EXPORTED_INVITES_FIRST_PAGE
    } else {
        EXPORTED_INVITES_PER_PAGE
    };
    let (offset_date, offset_link) = match &query.offset {
        Some((date, link)) if !link.is_empty() => (Some(*date), Some(link.clone())),
        _ => (None, None),
    };
    tl_fns::messages::GetExportedChatInvites {
        revoked: query.revoked,
        peer: input_peer_for(session, peer_id),
        admin_id: input_user_for(session, query.admin_id),
        offset_date,
        offset_link,
        limit,
    }
}

/// Invokes `messages.getExportedChatInvites`, ingests `users` into `session`, and returns
/// [`ExportedChatInvitesPage`].
pub async fn get_exported_chat_invites<R: RpcClient>(
    rpc: &R,
    session: &mut Session,
    peer_id: PeerId,
    query: &ExportedChatInvitesQuery,
) -> Result<ExportedChatInvitesPage, ApiError> {
    let req = build_get_exported_chat_invites(session, peer_id, query);
    let tl_enums::messages::ExportedChatInvites::ExportedChatInvites(resp) =
        invoke_api(rpc, &req).await?;
    apply_tl_users_and_chats(session, &resp.users, &[]);
    let invites: Vec<ExportedChatInviteInfo> = resp
        .invites
        .iter()
        .filter_map(exported_chat_invite_from_tl)
        .collect();
    let next_offset = invites.last().map(|last| (last.date, last.link.clone()));
    Ok(ExportedChatInvitesPage {
        count: resp.count,
        invites,
        next_offset,
    })
}

// ---------------------------------------------------------------------------
// Read History & Read Message Contents (`channels.readHistory`, `readMessageContents`)
// ---------------------------------------------------------------------------

/// Builds `channels.readHistory`.
#[must_use]
pub fn build_read_channel_history(
    channel: tl_enums::InputChannel,
    max_id: MsgId,
) -> tl_fns::channels::ReadHistory {
    tl_fns::channels::ReadHistory {
        channel,
        max_id: max_id.server_i32().unwrap_or(0),
    }
}

/// Invokes `channels.readHistory` for `(channel_id, max_id)`.
pub async fn read_channel_history<R: RpcClient>(
    rpc: &R,
    session: &Session,
    channel_id: ChannelId,
    max_id: MsgId,
) -> Result<bool, ApiError> {
    let req = build_read_channel_history(input_channel_for(session, channel_id), max_id);
    invoke_api(rpc, &req).await
}

/// Builds `channels.readMessageContents` (`ApiWrap::readServerHistory` / `sendReadContents`
/// in `apiwrap.cpp:1495-1527`).
#[must_use]
pub fn build_read_channel_message_contents(
    channel: tl_enums::InputChannel,
    ids: &[MsgId],
) -> tl_fns::channels::ReadMessageContents {
    tl_fns::channels::ReadMessageContents {
        channel,
        id: ids.iter().copied().filter_map(MsgId::server_i32).collect(),
    }
}

/// Invokes `channels.readMessageContents` (marks voice/video notes, mentions, TTL media read
/// in a channel/supergroup).
pub async fn read_channel_message_contents<R: RpcClient>(
    rpc: &R,
    session: &Session,
    channel_id: ChannelId,
    ids: &[MsgId],
) -> Result<bool, ApiError> {
    let req = build_read_channel_message_contents(input_channel_for(session, channel_id), ids);
    invoke_api(rpc, &req).await
}

/// Builds `messages.readMessageContents` (for non-channel dialogs; returns `AffectedMessages`
/// whose `pts`/`pts_count` must be fed into `UpdatesHandler`).
#[must_use]
pub fn build_read_common_message_contents(ids: &[MsgId]) -> tl_fns::messages::ReadMessageContents {
    tl_fns::messages::ReadMessageContents {
        id: ids.iter().copied().filter_map(MsgId::server_i32).collect(),
    }
}

/// Invokes `messages.readMessageContents`.
pub async fn read_common_message_contents<R: RpcClient>(
    rpc: &R,
    ids: &[MsgId],
) -> Result<tl_enums::messages::AffectedMessages, ApiError> {
    let req = build_read_common_message_contents(ids);
    invoke_api(rpc, &req).await
}

// ---------------------------------------------------------------------------
// Channel Recommendations (`channels.getChannelRecommendations`)
// ---------------------------------------------------------------------------

/// Parsed response of `channels.getChannelRecommendations`
/// (`Api::ChatParticipants::loadSimilarChannels` in `api/api_chat_participants.cpp:224-244, 869-938`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelRecommendations {
    /// Recommended channel/peer IDs in server order.
    pub peers: Vec<PeerId>,
    /// Total count reported by server (equals `peers.len()` for `messages.chats`,
    /// or `slice.count` for `messages.chatsSlice`).
    pub total_count: i32,
    /// Additional recommendations hidden behind Telegram Premium (`(total_count - peers.len()).max(0)`).
    pub more: i32,
}

/// Builds `channels.getChannelRecommendations`.
///
/// Pass `channel: Some(...)` for channels similar to a specific channel, or `None` for
/// global account recommendations.
#[must_use]
pub fn build_get_channel_recommendations(
    channel: Option<tl_enums::InputChannel>,
) -> tl_fns::channels::GetChannelRecommendations {
    tl_fns::channels::GetChannelRecommendations { channel }
}

/// Invokes `channels.getChannelRecommendations`, ingests returned `chats` into `session`, and
/// computes `total_count` and `more` matching `api/api_chat_participants.cpp:224-244`.
pub async fn get_channel_recommendations<R: RpcClient>(
    rpc: &R,
    session: &mut Session,
    channel_id: Option<ChannelId>,
) -> Result<ChannelRecommendations, ApiError> {
    let req =
        build_get_channel_recommendations(channel_id.map(|cid| input_channel_for(session, cid)));
    let resp = invoke_api(rpc, &req).await?;
    let (chats, total_count) = match resp {
        tl_enums::messages::Chats::Chats(c) => {
            let count = c.chats.len() as i32;
            (c.chats, count)
        }
        tl_enums::messages::Chats::Slice(s) => (s.chats, s.count),
    };
    let mut peers = Vec::with_capacity(chats.len());
    for chat in &chats {
        if let Some(pid) = crate::convert::apply_tl_chat(session, chat) {
            peers.push(pid);
        }
    }
    let more = (total_count - peers.len() as i32).max(0);
    Ok(ChannelRecommendations {
        peers,
        total_count,
        more,
    })
}
