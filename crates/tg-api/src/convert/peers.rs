//! Peer, user, chat, channel, and `InputPeer` / `access_hash` conversions.
//!
//! Port origin: `data/data_session.cpp` (`processUser`, `processChat`),
//! `data/data_user.cpp`, `data/data_chat.cpp`, `data/data_channel.cpp`.

use tg_data::{
    AllowedReactions, AllowedReactionsType, Birthday, BlockStatus, BotCommand, BotInfo,
    BotVerifyDetails, CallId, CallsStatus, Channel, ChannelFlags, ChannelId, ChannelLocation, Chat,
    ChatAdminRights, ChatBannedRights, ChatFlags, ChatId, ChatRestrictionsInfo, CollectibleId,
    ContactStatus, DialogKey, DocumentId, EmojiStatus, LastseenStatus, LoadedStatus, MsgId,
    PeerBarSettings, PeerCommon, PeerId, PeerNotifySettings, PeerUpdateFlags, PhotoId, ProfileTab,
    ReactionId, Session, StarsRating, UnavailableReason, User, UserFlags, UserId, UserStatusInput,
    Username,
};
use tg_tl::{enums as tl_enums, types as tl_types};

use crate::convert::media::{geo_point_from_tl, photo_from_tl, photo_id_from_tl, reaction_from_tl};
use crate::convert::messages::text_with_entities_from_tl;

/// Converts a TL `Peer` into a `tg_data::PeerId`.
#[must_use]
pub const fn peer_id_from_tl(peer: &tl_enums::Peer) -> PeerId {
    match peer {
        tl_enums::Peer::User(u) => PeerId::from_user(UserId::from_tl(u.user_id)),
        tl_enums::Peer::Chat(c) => PeerId::from_chat(ChatId::from_tl(c.chat_id)),
        tl_enums::Peer::Channel(ch) => PeerId::from_channel(ChannelId::from_tl(ch.channel_id)),
    }
}

/// Converts a `tg_data::PeerId` into a TL `Peer` (returns `None` for zero or fake peer IDs).
#[must_use]
pub const fn peer_to_tl(peer_id: PeerId) -> Option<tl_enums::Peer> {
    if peer_id.is_zero() || peer_id.is_fake() {
        return None;
    }
    if let Some(uid) = peer_id.to_user() {
        Some(tl_enums::Peer::User(tl_types::PeerUser {
            user_id: uid.to_tl(),
        }))
    } else if let Some(cid) = peer_id.to_chat() {
        Some(tl_enums::Peer::Chat(tl_types::PeerChat {
            chat_id: cid.to_tl(),
        }))
    } else if let Some(chid) = peer_id.to_channel() {
        Some(tl_enums::Peer::Channel(tl_types::PeerChannel {
            channel_id: chid.to_tl(),
        }))
    } else {
        None
    }
}

/// Extracts a `tg_data::PeerId` from a TL `InputPeer`.
///
/// Uses `self_id` when the input peer is `InputPeer::Self_`.
#[must_use]
pub const fn peer_id_from_input_peer(
    peer: &tl_enums::InputPeer,
    self_id: Option<UserId>,
) -> PeerId {
    match peer {
        tl_enums::InputPeer::Empty => PeerId::ZERO,
        tl_enums::InputPeer::Self_ => match self_id {
            Some(uid) => PeerId::from_user(uid),
            None => PeerId::ZERO,
        },
        tl_enums::InputPeer::User(u) => PeerId::from_user(UserId::from_tl(u.user_id)),
        tl_enums::InputPeer::UserFromMessage(u) => PeerId::from_user(UserId::from_tl(u.user_id)),
        tl_enums::InputPeer::Chat(c) => PeerId::from_chat(ChatId::from_tl(c.chat_id)),
        tl_enums::InputPeer::Channel(ch) => PeerId::from_channel(ChannelId::from_tl(ch.channel_id)),
        tl_enums::InputPeer::ChannelFromMessage(ch) => {
            PeerId::from_channel(ChannelId::from_tl(ch.channel_id))
        }
    }
}

/// Converts a TL `DialogPeer` into a `tg_data::DialogKey`.
#[must_use]
pub const fn dialog_key_from_tl(peer: &tl_enums::DialogPeer) -> Option<DialogKey> {
    match peer {
        tl_enums::DialogPeer::DialogPeer(d) => Some(DialogKey::History(peer_id_from_tl(&d.peer))),
        tl_enums::DialogPeer::Folder(f) => Some(DialogKey::Folder(f.folder_id)),
        tl_enums::DialogPeer::Community(c) => Some(DialogKey::History(PeerId::from_channel(
            ChannelId::from_tl(c.community_id),
        ))),
    }
}

/// Extracts a TL `InputChannel` if `peer` refers to a channel (`InputPeer::Channel` or
/// `InputPeer::ChannelFromMessage`).
#[must_use]
pub fn input_peer_to_input_channel(peer: &tl_enums::InputPeer) -> Option<tl_enums::InputChannel> {
    match peer {
        tl_enums::InputPeer::Channel(ch) => Some(tl_enums::InputChannel::InputChannel(
            tl_types::InputChannel {
                channel_id: ch.channel_id,
                access_hash: ch.access_hash,
            },
        )),
        tl_enums::InputPeer::ChannelFromMessage(ch) => Some(tl_enums::InputChannel::FromMessage(
            tl_types::InputChannelFromMessage {
                peer: (*ch.peer).clone(),
                msg_id: ch.msg_id,
                channel_id: ch.channel_id,
            },
        )),
        _ => None,
    }
}

/// Builds a TL `InputPeer` for `peer_id` using the access hashes and `min`-peer message
/// references tracked in `session` (`PeerData::input` in `data/data_peer.cpp:2078-2111`).
///
/// When only a `min` user or channel is known (`!session.peer_loaded(peer_id)`), falls back
/// to `InputPeer::UserFromMessage` / `InputPeer::ChannelFromMessage` via
/// `Session::message_with_peer`.
#[must_use]
pub fn input_peer_for(session: &Session, peer_id: PeerId) -> tl_enums::InputPeer {
    if peer_id.is_zero() || peer_id.is_fake() {
        return tl_enums::InputPeer::Empty;
    }
    if let Some(uid) = peer_id.to_user() {
        match input_user_for(session, uid) {
            tl_enums::InputUser::Empty => tl_enums::InputPeer::Empty,
            tl_enums::InputUser::Self_ => tl_enums::InputPeer::Self_,
            tl_enums::InputUser::InputUser(u) => {
                tl_enums::InputPeer::User(tl_types::InputPeerUser {
                    user_id: u.user_id,
                    access_hash: u.access_hash,
                })
            }
            tl_enums::InputUser::FromMessage(u) => {
                tl_enums::InputPeer::UserFromMessage(tl_types::InputPeerUserFromMessage {
                    peer: Box::new(u.peer),
                    msg_id: u.msg_id,
                    user_id: u.user_id,
                })
            }
        }
    } else if let Some(cid) = peer_id.to_chat() {
        tl_enums::InputPeer::Chat(tl_types::InputPeerChat {
            chat_id: cid.to_tl(),
        })
    } else if let Some(chid) = peer_id.to_channel() {
        match input_channel_for(session, chid) {
            tl_enums::InputChannel::Empty => tl_enums::InputPeer::Empty,
            tl_enums::InputChannel::InputChannel(ch) => {
                tl_enums::InputPeer::Channel(tl_types::InputPeerChannel {
                    channel_id: ch.channel_id,
                    access_hash: ch.access_hash,
                })
            }
            tl_enums::InputChannel::FromMessage(ch) => {
                tl_enums::InputPeer::ChannelFromMessage(tl_types::InputPeerChannelFromMessage {
                    peer: Box::new(ch.peer),
                    msg_id: ch.msg_id,
                    channel_id: ch.channel_id,
                })
            }
        }
    } else {
        tl_enums::InputPeer::Empty
    }
}

/// Builds a TL `InputUser` for `user_id` using the access hashes and `min`-peer message
/// references tracked in `session` (`UserData::inputUser` in `data/data_user.cpp:390-406`).
#[must_use]
pub fn input_user_for(session: &Session, user_id: UserId) -> tl_enums::InputUser {
    if user_id.is_zero() {
        return tl_enums::InputUser::Empty;
    }
    let peer_id = PeerId::from_user(user_id);
    if !session.user_loaded(user_id)
        && let Some(full_id) = session.message_with_peer(peer_id)
        && (full_id.peer.is_chat() || session.peer_loaded(full_id.peer))
    {
        return tl_enums::InputUser::FromMessage(tl_types::InputUserFromMessage {
            peer: input_peer_for(session, full_id.peer),
            msg_id: full_id.msg.0 as i32,
            user_id: user_id.to_tl(),
        });
    }
    if user_id == session.self_user_id() {
        return tl_enums::InputUser::Self_;
    }
    let access_hash = session.peer_access_hash(peer_id).unwrap_or(0);
    tl_enums::InputUser::InputUser(tl_types::InputUser {
        user_id: user_id.to_tl(),
        access_hash,
    })
}

/// Builds a TL `InputChannel` for `channel_id` using the access hashes and `min`-peer message
/// references tracked in `session` (`ChannelData::inputChannel` in
/// `data/data_channel.cpp:1299-1310`).
#[must_use]
pub fn input_channel_for(session: &Session, channel_id: ChannelId) -> tl_enums::InputChannel {
    if channel_id.is_zero() {
        return tl_enums::InputChannel::Empty;
    }
    let peer_id = PeerId::from_channel(channel_id);
    if !session.channel_loaded(channel_id)
        && let Some(full_id) = session.message_with_peer(peer_id)
        && (full_id.peer.is_chat() || session.peer_loaded(full_id.peer))
    {
        return tl_enums::InputChannel::FromMessage(tl_types::InputChannelFromMessage {
            peer: input_peer_for(session, full_id.peer),
            msg_id: full_id.msg.0 as i32,
            channel_id: channel_id.to_tl(),
        });
    }
    let access_hash = session.peer_access_hash(peer_id).unwrap_or(0);
    tl_enums::InputChannel::InputChannel(tl_types::InputChannel {
        channel_id: channel_id.to_tl(),
        access_hash,
    })
}

/// Builds a TL `InputDialogPeer` for `key` using the access hashes tracked in `session`.
#[must_use]
pub fn input_dialog_peer_for(
    session: &Session,
    key: DialogKey,
) -> Option<tl_enums::InputDialogPeer> {
    match key {
        DialogKey::History(peer_id) => Some(tl_enums::InputDialogPeer::InputDialogPeer(
            tl_types::InputDialogPeer {
                peer: input_peer_for(session, peer_id),
            },
        )),
        DialogKey::Folder(folder_id) => Some(tl_enums::InputDialogPeer::Folder(
            tl_types::InputDialogPeerFolder { folder_id },
        )),
        DialogKey::ForumTopic { .. } | DialogKey::SavedSublist { .. } => None,
    }
}

/// Converts a TL `UserStatus` into `tg_data::UserStatusInput`.
#[must_use]
pub const fn user_status_from_tl(status: &tl_enums::UserStatus) -> UserStatusInput {
    match status {
        tl_enums::UserStatus::Empty => UserStatusInput::Empty,
        tl_enums::UserStatus::Online(s) => UserStatusInput::Online { expires: s.expires },
        tl_enums::UserStatus::Offline(s) => UserStatusInput::Offline {
            was_online: s.was_online,
        },
        tl_enums::UserStatus::Recently(s) => UserStatusInput::Recently { by_me: s.by_me },
        tl_enums::UserStatus::LastWeek(s) => UserStatusInput::LastWeek { by_me: s.by_me },
        tl_enums::UserStatus::LastMonth(s) => UserStatusInput::LastMonth { by_me: s.by_me },
    }
}

/// Converts TL `ChatAdminRights` into `tg_data::ChatAdminRights`.
#[must_use]
pub fn admin_rights_from_tl(rights: &tl_enums::ChatAdminRights) -> ChatAdminRights {
    let tl_enums::ChatAdminRights::ChatAdminRights(r) = rights;
    let mut out = ChatAdminRights::empty();
    out.set(ChatAdminRights::CHANGE_INFO, r.change_info);
    out.set(ChatAdminRights::POST_MESSAGES, r.post_messages);
    out.set(ChatAdminRights::EDIT_MESSAGES, r.edit_messages);
    out.set(ChatAdminRights::DELETE_MESSAGES, r.delete_messages);
    out.set(ChatAdminRights::BAN_USERS, r.ban_users);
    out.set(ChatAdminRights::INVITE_USERS, r.invite_users);
    out.set(ChatAdminRights::PIN_MESSAGES, r.pin_messages);
    out.set(ChatAdminRights::ADD_ADMINS, r.add_admins);
    out.set(ChatAdminRights::ANONYMOUS, r.anonymous);
    out.set(ChatAdminRights::MANAGE_CALL, r.manage_call);
    out.set(ChatAdminRights::OTHER, r.other);
    out.set(ChatAdminRights::MANAGE_TOPICS, r.manage_topics);
    out.set(ChatAdminRights::POST_STORIES, r.post_stories);
    out.set(ChatAdminRights::EDIT_STORIES, r.edit_stories);
    out.set(ChatAdminRights::DELETE_STORIES, r.delete_stories);
    out.set(
        ChatAdminRights::MANAGE_DIRECT_MESSAGES,
        r.manage_direct_messages,
    );
    out.set(ChatAdminRights::MANAGE_RANKS, r.manage_ranks);
    out.set(ChatAdminRights::MANAGE_LINKED_PEERS, r.manage_linked_peers);
    out.set(
        ChatAdminRights::MANAGE_WELCOME_MESSAGES,
        r.manage_welcome_messages,
    );
    out
}

/// Converts `tg_data::ChatAdminRights` into TL `ChatAdminRights` (`AdminRightsToMTP`
/// in `data/data_chat_participant_status.cpp:98-140`).
#[must_use]
pub fn admin_rights_to_tl(rights: ChatAdminRights) -> tl_enums::ChatAdminRights {
    tl_enums::ChatAdminRights::ChatAdminRights(tl_types::ChatAdminRights {
        change_info: rights.contains(ChatAdminRights::CHANGE_INFO),
        post_messages: rights.contains(ChatAdminRights::POST_MESSAGES),
        edit_messages: rights.contains(ChatAdminRights::EDIT_MESSAGES),
        delete_messages: rights.contains(ChatAdminRights::DELETE_MESSAGES),
        ban_users: rights.contains(ChatAdminRights::BAN_USERS),
        invite_users: rights.contains(ChatAdminRights::INVITE_USERS),
        pin_messages: rights.contains(ChatAdminRights::PIN_MESSAGES),
        add_admins: rights.contains(ChatAdminRights::ADD_ADMINS),
        anonymous: rights.contains(ChatAdminRights::ANONYMOUS),
        manage_call: rights.contains(ChatAdminRights::MANAGE_CALL),
        other: rights.contains(ChatAdminRights::OTHER),
        manage_topics: rights.contains(ChatAdminRights::MANAGE_TOPICS),
        post_stories: rights.contains(ChatAdminRights::POST_STORIES),
        edit_stories: rights.contains(ChatAdminRights::EDIT_STORIES),
        delete_stories: rights.contains(ChatAdminRights::DELETE_STORIES),
        manage_direct_messages: rights.contains(ChatAdminRights::MANAGE_DIRECT_MESSAGES),
        manage_ranks: rights.contains(ChatAdminRights::MANAGE_RANKS),
        manage_linked_peers: rights.contains(ChatAdminRights::MANAGE_LINKED_PEERS),
        manage_welcome_messages: rights.contains(ChatAdminRights::MANAGE_WELCOME_MESSAGES),
    })
}

/// Converts TL `ChatBannedRights` into `tg_data::ChatRestrictionsInfo`.
#[must_use]
pub fn banned_rights_from_tl(rights: &tl_enums::ChatBannedRights) -> ChatRestrictionsInfo {
    let tl_enums::ChatBannedRights::ChatBannedRights(r) = rights;
    let mut flags = ChatBannedRights::empty();
    flags.set(ChatBannedRights::VIEW_MESSAGES, r.view_messages);
    flags.set(ChatBannedRights::SEND_MESSAGES, r.send_messages);
    flags.set(ChatBannedRights::SEND_MEDIA, r.send_media);
    flags.set(ChatBannedRights::SEND_STICKERS, r.send_stickers);
    flags.set(ChatBannedRights::SEND_GIFS, r.send_gifs);
    flags.set(ChatBannedRights::SEND_GAMES, r.send_games);
    flags.set(ChatBannedRights::SEND_INLINE, r.send_inline);
    flags.set(ChatBannedRights::EMBED_LINKS, r.embed_links);
    flags.set(ChatBannedRights::SEND_POLLS, r.send_polls);
    flags.set(ChatBannedRights::CHANGE_INFO, r.change_info);
    flags.set(ChatBannedRights::INVITE_USERS, r.invite_users);
    flags.set(ChatBannedRights::PIN_MESSAGES, r.pin_messages);
    flags.set(ChatBannedRights::MANAGE_TOPICS, r.manage_topics);
    flags.set(ChatBannedRights::SEND_PHOTOS, r.send_photos);
    flags.set(ChatBannedRights::SEND_VIDEOS, r.send_videos);
    flags.set(ChatBannedRights::SEND_ROUNDVIDEOS, r.send_roundvideos);
    flags.set(ChatBannedRights::SEND_AUDIOS, r.send_audios);
    flags.set(ChatBannedRights::SEND_VOICES, r.send_voices);
    flags.set(ChatBannedRights::SEND_DOCS, r.send_docs);
    flags.set(ChatBannedRights::SEND_PLAIN, r.send_plain);
    flags.set(ChatBannedRights::EDIT_RANK, r.edit_rank);
    flags.set(ChatBannedRights::SEND_REACTIONS, r.send_reactions);
    flags.set(ChatBannedRights::MANAGE_LINKED_PEERS, r.manage_linked_peers);
    ChatRestrictionsInfo::new(flags, r.until_date)
}

/// Converts `tg_data::ChatRestrictionsInfo` into TL `ChatBannedRights` (`RestrictionsToMTP`
/// in `data/data_chat_participant_status.cpp:147-177`).
///
/// Matches tdesktop by never setting the legacy coarse `send_messages` / `send_media` flags;
/// modern granular bits (`send_plain`, `send_photos`, `send_videos`, etc.) are sent instead.
#[must_use]
pub fn banned_rights_to_tl(info: ChatRestrictionsInfo) -> tl_enums::ChatBannedRights {
    let f = info.flags;
    tl_enums::ChatBannedRights::ChatBannedRights(tl_types::ChatBannedRights {
        view_messages: f.contains(ChatBannedRights::VIEW_MESSAGES),
        send_messages: false,
        send_media: false,
        send_stickers: f.contains(ChatBannedRights::SEND_STICKERS),
        send_gifs: f.contains(ChatBannedRights::SEND_GIFS),
        send_games: f.contains(ChatBannedRights::SEND_GAMES),
        send_inline: f.contains(ChatBannedRights::SEND_INLINE),
        embed_links: f.contains(ChatBannedRights::EMBED_LINKS),
        send_polls: f.contains(ChatBannedRights::SEND_POLLS),
        change_info: f.contains(ChatBannedRights::CHANGE_INFO),
        invite_users: f.contains(ChatBannedRights::INVITE_USERS),
        pin_messages: f.contains(ChatBannedRights::PIN_MESSAGES),
        manage_topics: f.contains(ChatBannedRights::MANAGE_TOPICS),
        send_photos: f.contains(ChatBannedRights::SEND_PHOTOS),
        send_videos: f.contains(ChatBannedRights::SEND_VIDEOS),
        send_roundvideos: f.contains(ChatBannedRights::SEND_ROUNDVIDEOS),
        send_audios: f.contains(ChatBannedRights::SEND_AUDIOS),
        send_voices: f.contains(ChatBannedRights::SEND_VOICES),
        send_docs: f.contains(ChatBannedRights::SEND_DOCS),
        send_plain: f.contains(ChatBannedRights::SEND_PLAIN),
        edit_rank: f.contains(ChatBannedRights::EDIT_RANK),
        send_reactions: f.contains(ChatBannedRights::SEND_REACTIONS),
        manage_linked_peers: f.contains(ChatBannedRights::MANAGE_LINKED_PEERS),
        until_date: info.until,
    })
}

/// Converts TL `PeerNotifySettings` into `tg_data::PeerNotifySettings`.
///
/// Matches `NotifyPeerSettingsValue::change` (`data/notify/data_peer_notify_settings.cpp:108-124`):
/// desktop only reads `other_sound` and does NOT fall back to `ios_sound` or `android_sound`
/// (platform-specific iOS/Android sound identifiers must not override desktop defaults).
#[must_use]
pub fn peer_notify_settings_from_tl(settings: &tl_enums::PeerNotifySettings) -> PeerNotifySettings {
    let tl_enums::PeerNotifySettings::PeerNotifySettings(s) = settings;
    let sound = s.other_sound.as_ref().map(|snd| match snd {
        tl_enums::NotificationSound::Default => "default".to_owned(),
        tl_enums::NotificationSound::None => "none".to_owned(),
        tl_enums::NotificationSound::Local(l) => l.data.clone(),
        tl_enums::NotificationSound::Ringtone(r) => format!("ringtone:{}", r.id),
    });
    PeerNotifySettings {
        mute_until: s.mute_until,
        silent: s.silent,
        show_previews: s.show_previews,
        sound,
        stories_muted: s.stories_muted,
    }
}

fn apply_peer_color(
    common: &mut PeerCommon,
    color: Option<&tl_enums::PeerColor>,
    is_profile: bool,
) {
    match color {
        Some(tl_enums::PeerColor::PeerColor(c)) => {
            if let Some(idx) = c.color {
                if is_profile {
                    common.set_color_profile_index(idx as u8);
                } else {
                    common.set_color_index(idx as u8);
                }
            } else if is_profile {
                common.clear_color_profile_index();
            } else {
                common.clear_color_index();
            }
            let bg = c
                .background_emoji_id
                .map(DocumentId::from_tl)
                .unwrap_or(DocumentId::ZERO);
            if is_profile {
                common.profile_background_emoji_id = bg;
            } else {
                common.background_emoji_id = bg;
            }
        }
        Some(tl_enums::PeerColor::Collectible(c)) => {
            let bg = DocumentId::from_tl(c.background_emoji_id);
            if is_profile {
                common.profile_background_emoji_id = bg;
            } else {
                common.background_emoji_id = bg;
            }
        }
        Some(tl_enums::PeerColor::InputPeerColorCollectible(_)) | None => {
            if is_profile {
                common.clear_color_profile_index();
                common.profile_background_emoji_id = DocumentId::ZERO;
            } else {
                common.clear_color_index();
                common.background_emoji_id = DocumentId::ZERO;
            }
        }
    }
}

/// Converts an optional TL `EmojiStatus` into [`EmojiStatus`] (`Data::EmojiStatuses::parse`,
/// `data/data_emoji_statuses.cpp:116-138`).
#[must_use]
pub fn emoji_status_from_tl(status: Option<&tl_enums::EmojiStatus>) -> EmojiStatus {
    match status {
        Some(tl_enums::EmojiStatus::EmojiStatus(s)) => EmojiStatus {
            document_id: DocumentId::from_tl(s.document_id),
            collectible_id: None,
            until: s.until.unwrap_or(0),
        },
        Some(tl_enums::EmojiStatus::Collectible(c)) => EmojiStatus {
            document_id: DocumentId::from_tl(c.document_id),
            collectible_id: Some(CollectibleId::from_tl(c.collectible_id)),
            until: c.until.unwrap_or(0),
        },
        _ => EmojiStatus::default(),
    }
}

fn restriction_reasons_from_tl(
    reasons: Option<&[tl_enums::RestrictionReason]>,
) -> Vec<UnavailableReason> {
    reasons
        .unwrap_or_default()
        .iter()
        .map(|r| {
            let tl_enums::RestrictionReason::RestrictionReason(rr) = r;
            UnavailableReason {
                platform: rr.platform.clone(),
                reason: rr.reason.clone(),
                text: rr.text.clone(),
            }
        })
        .collect()
}

fn usernames_from_tl(list: &[tl_enums::Username]) -> Vec<Username> {
    list.iter()
        .map(|u| {
            let tl_enums::Username::Username(u) = u;
            Username {
                username: u.username.clone(),
                active: u.active,
                editable: u.editable,
            }
        })
        .collect()
}

fn profile_tab_from_tl(tab: Option<&tl_enums::ProfileTab>) -> ProfileTab {
    match tab {
        None => ProfileTab::None,
        Some(tl_enums::ProfileTab::Posts) => ProfileTab::Posts,
        Some(tl_enums::ProfileTab::Gifts) => ProfileTab::Gifts,
        Some(tl_enums::ProfileTab::Media) => ProfileTab::Media,
        Some(tl_enums::ProfileTab::Files) => ProfileTab::Files,
        Some(tl_enums::ProfileTab::Music) => ProfileTab::Music,
        Some(tl_enums::ProfileTab::Voice) => ProfileTab::Voice,
        Some(tl_enums::ProfileTab::Links) => ProfileTab::Links,
        Some(tl_enums::ProfileTab::Gifs) => ProfileTab::Gifs,
    }
}

fn allowed_reactions_from_tl(
    reactions: Option<&tl_enums::ChatReactions>,
    limit: Option<i32>,
    paid_enabled: bool,
) -> AllowedReactions {
    let max_count = limit.unwrap_or(0);
    match reactions {
        None | Some(tl_enums::ChatReactions::None) => AllowedReactions {
            some: Vec::new(),
            max_count,
            kind: AllowedReactionsType::Some,
            paid_enabled,
        },
        Some(tl_enums::ChatReactions::All(a)) => AllowedReactions {
            some: Vec::new(),
            max_count,
            kind: if a.allow_custom {
                AllowedReactionsType::All
            } else {
                AllowedReactionsType::Default
            },
            paid_enabled,
        },
        Some(tl_enums::ChatReactions::Some(s)) => AllowedReactions {
            some: s
                .reactions
                .iter()
                .filter_map(reaction_from_tl)
                .collect::<Vec<ReactionId>>(),
            max_count,
            kind: AllowedReactionsType::Some,
            paid_enabled,
        },
    }
}

fn bot_verification_from_tl(v: &tl_enums::BotVerification) -> BotVerifyDetails {
    let tl_enums::BotVerification::BotVerification(bv) = v;
    BotVerifyDetails {
        bot_id: UserId::from_tl(bv.bot_id),
        icon_id: DocumentId::from_tl(bv.icon),
        description: text_with_entities_from_tl(&bv.description).text,
    }
}

fn peer_bar_settings_from_tl(settings: &tl_enums::PeerSettings) -> PeerBarSettings {
    let tl_enums::PeerSettings::PeerSettings(s) = settings;
    let mut out = PeerBarSettings::empty();
    out.set(PeerBarSettings::REPORT_SPAM, s.report_spam);
    out.set(PeerBarSettings::ADD_CONTACT, s.add_contact);
    out.set(PeerBarSettings::BLOCK_CONTACT, s.block_contact);
    out.set(PeerBarSettings::SHARE_CONTACT, s.share_contact);
    out.set(
        PeerBarSettings::NEED_CONTACTS_EXCEPTION,
        s.need_contacts_exception,
    );
    out.set(PeerBarSettings::AUTO_ARCHIVED, s.autoarchived);
    out.set(
        PeerBarSettings::REQUEST_CHAT,
        s.request_chat_title.is_some(),
    );
    out.set(
        PeerBarSettings::REQUEST_CHAT_BROADCAST,
        s.request_chat_broadcast,
    );
    out.set(
        PeerBarSettings::HAS_BUSINESS_BOT,
        s.business_bot_id.is_some(),
    );
    out.set(PeerBarSettings::BUSINESS_BOT_PAUSED, s.business_bot_paused);
    out.set(
        PeerBarSettings::BUSINESS_BOT_CAN_REPLY,
        s.business_bot_can_reply,
    );
    out
}

fn chat_theme_token_from_tl(theme: Option<&tl_enums::ChatTheme>) -> String {
    match theme {
        Some(tl_enums::ChatTheme::ChatTheme(t)) => t.emoticon.clone(),
        Some(tl_enums::ChatTheme::UniqueGift(g)) => match &g.gift {
            tl_enums::StarGift::Unique(u) => u.slug.clone(),
            tl_enums::StarGift::StarGift(s) => s.title.clone().unwrap_or_default(),
        },
        None => String::new(),
    }
}

/// Applies a TL `User` (`user` or `userEmpty`) into `session`, faithfully porting
/// `Data::Session::processUser` (`data/data_session.cpp:676-966`) including `min` merge rules,
/// `userEmpty` clearing, `DISCARD_MIN_PHOTO`, and `access_hash` bookkeeping.
pub fn apply_tl_user(session: &mut Session, tl_user: &tl_enums::User) -> Option<PeerId> {
    match tl_user {
        tl_enums::User::Empty(empty) => {
            let uid = UserId::from_tl(empty.id);
            if uid.is_zero() {
                return None;
            }
            let peer_id = PeerId::from_user(uid);
            let mut user = session
                .user(uid)
                .cloned()
                .unwrap_or_else(|| User::new(uid, uid == session.self_user_id()));
            // Match `Session::processUser(MTPDuserEmpty)` (`data/data_session.cpp:686-701`):
            // reset name to Deleted Account, clear photo, phone, usernames, bot info, flags,
            // contact status, and set lastseen to empty/long-ago.
            let mut changed = PeerUpdateFlags::NAME | PeerUpdateFlags::PHOTO;
            if !user.phone.is_empty() {
                user.phone.clear();
                changed |= PeerUpdateFlags::PHONE_NUMBER;
            }
            user.usernames.set_usernames(&[]);
            user.set_name("Deleted Account", "", "", "");
            user.common.userpic_photo_id = PhotoId::ZERO;
            user.common.userpic_has_video = false;
            user.flags = UserFlags::DELETED;
            user.bot_info = None;
            user.contact_status = ContactStatus::NotContact;
            user.lastseen = LastseenStatus::long_ago(false);
            user.common.loaded_status = LoadedStatus::Normal;
            session.upsert_user(
                user,
                changed
                    | PeerUpdateFlags::IS_CONTACT
                    | PeerUpdateFlags::IS_BOT
                    | PeerUpdateFlags::ONLINE_STATUS,
            );
            Some(peer_id)
        }
        tl_enums::User::User(d) => {
            let uid = UserId::from_tl(d.id);
            if uid.is_zero() {
                return None;
            }
            let peer_id = PeerId::from_user(uid);
            let is_self = d.self_ || uid == session.self_user_id();
            let mut user = session
                .user(uid)
                .cloned()
                .unwrap_or_else(|| User::new(uid, is_self));

            let is_min = d.min;

            let mut changed = PeerUpdateFlags::empty();

            if !is_min {
                if let Some(hash) = d.access_hash {
                    user.set_access_hash(hash as u64);
                    session.set_peer_access_hash(peer_id, hash);
                }
            } else if user.access_hash == 0
                && let Some(hash) = d.access_hash
                && hash != 0
            {
                // Preserve non-zero hash on first minimal load if we didn't have one yet
                // (`data/data_session.cpp:828-832`).
                user.set_access_hash(hash as u64);
                session.set_peer_access_hash(peer_id, hash);
            }

            // Flags (`data/data_session.cpp:744-809`).
            user.flags.set(UserFlags::SELF, is_self);
            user.flags.set(UserFlags::DELETED, d.deleted);
            user.flags.set(UserFlags::VERIFIED, d.verified);
            user.flags.set(UserFlags::SCAM, d.scam);
            user.flags.set(UserFlags::FAKE, d.fake);
            user.flags.set(UserFlags::SUPPORT, d.support);
            user.flags.set(UserFlags::PREMIUM, d.premium);
            user.flags.set(UserFlags::BOT_INLINE_GEO, d.bot_inline_geo);
            user.flags.set(UserFlags::FORUM, d.bot_forum_view);
            user.flags.set(
                UserFlags::HAS_REQUIRE_PREMIUM_TO_WRITE,
                d.contact_require_premium,
            );
            if !is_min {
                user.flags.set(UserFlags::CONTACT, d.contact);
                user.flags.set(UserFlags::MUTUAL_CONTACT, d.mutual_contact);
                // Track `DISCARD_MIN_PHOTO` from full user (`data/data_session.cpp:803-805`).
                user.flags
                    .set(UserFlags::DISCARD_MIN_PHOTO, !d.apply_min_photo);
                user.flags.set(UserFlags::CLOSE_FRIEND, d.close_friend);
                user.flags.set(UserFlags::STORIES_HIDDEN, d.stories_hidden);
                user.contact_status = if d.contact || d.mutual_contact {
                    ContactStatus::Contact
                } else {
                    ContactStatus::NotContact
                };
            }

            // Name, phone, usernames & profile photo (`data/data_session.cpp:833-918`).
            if d.deleted {
                if !user.phone.is_empty() {
                    user.phone.clear();
                    changed |= PeerUpdateFlags::PHONE_NUMBER;
                }
                user.usernames.set_usernames(&[]);
                user.set_name("Deleted Account", "", "", "");
                user.common.userpic_photo_id = PhotoId::ZERO;
                user.common.userpic_has_video = false;
                user.flags.remove(UserFlags::PERSONAL_PHOTO);
                changed |= PeerUpdateFlags::NAME | PeerUpdateFlags::PHOTO;
            } else {
                // Apply `first_name`/`last_name` from a `min` user only if we have no local
                // name yet (`data/data_session.cpp:842-862`); never overwrite `phone` or
                // `username` from a `min` user.
                let no_local_name = user.first_name.is_empty() && user.last_name.is_empty();
                let first = if !is_min || no_local_name {
                    d.first_name.as_deref().unwrap_or("").to_owned()
                } else {
                    user.first_name.clone()
                };
                let last = if !is_min || no_local_name {
                    d.last_name.as_deref().unwrap_or("").to_owned()
                } else {
                    user.last_name.clone()
                };
                let phone = if is_min {
                    user.phone.clone()
                } else {
                    d.phone.as_deref().unwrap_or("").to_owned()
                };
                let editable_username = if is_min {
                    user.usernames.editable_username().to_owned()
                } else {
                    d.username.as_deref().unwrap_or("").to_owned()
                };
                if user.phone != phone {
                    user.phone = phone.clone();
                    changed |= PeerUpdateFlags::PHONE_NUMBER;
                }
                let name_or_phone = if !first.is_empty() || !last.is_empty() {
                    if last.is_empty() {
                        first.clone()
                    } else if first.is_empty() {
                        last.clone()
                    } else {
                        format!("{first} {last}")
                    }
                } else if !user.phone.is_empty() {
                    user.phone.clone()
                } else {
                    String::new()
                };
                user.set_name(&first, &last, name_or_phone, &editable_username);
                if !is_min {
                    // Always sync full active username list on non-min user so removed
                    // collectible usernames are cleared (`data/data_session.cpp:907-917`).
                    if let Some(list) = &d.usernames {
                        user.usernames.set_usernames(&usernames_from_tl(list));
                    } else if !editable_username.is_empty() {
                        user.usernames.set_usernames(&[Username {
                            username: editable_username,
                            active: true,
                            editable: true,
                        }]);
                    } else {
                        user.usernames.set_usernames(&[]);
                    }
                    user.rebuild_name_index();
                    changed |= PeerUpdateFlags::USERNAME;
                }
                changed |= PeerUpdateFlags::NAME;

                // Profile photo (`data/data_session.cpp:899-905`):
                // apply when non-min, or when min and `DISCARD_MIN_PHOTO` is not set.
                if !is_min || !user.flags.contains(UserFlags::DISCARD_MIN_PHOTO) {
                    match &d.photo {
                        Some(tl_enums::UserProfilePhoto::UserProfilePhoto(p)) => {
                            user.common.userpic_photo_id = PhotoId::from_tl(p.photo_id);
                            user.common.userpic_has_video = p.has_video;
                            user.flags.set(UserFlags::PERSONAL_PHOTO, p.personal);
                            changed |= PeerUpdateFlags::PHOTO;
                        }
                        Some(tl_enums::UserProfilePhoto::Empty) | None => {
                            user.common.userpic_photo_id = PhotoId::ZERO;
                            user.common.userpic_has_video = false;
                            user.flags.remove(UserFlags::PERSONAL_PHOTO);
                            changed |= PeerUpdateFlags::PHOTO;
                        }
                    }
                }
            }

            // Status (never overwrite non-min status from a min user; `data/data_session.cpp:953-960`).
            if !is_min {
                if d.deleted {
                    user.lastseen = LastseenStatus::long_ago(false);
                    changed |= PeerUpdateFlags::ONLINE_STATUS;
                } else if let Some(status) = &d.status {
                    user.lastseen =
                        LastseenStatus::from_api(user_status_from_tl(status), user.lastseen);
                    changed |= PeerUpdateFlags::ONLINE_STATUS;
                } else {
                    user.lastseen = LastseenStatus::long_ago(false);
                    changed |= PeerUpdateFlags::ONLINE_STATUS;
                }
            }

            // Stories flags (`Data::Session::processUser` in `data/data_session.cpp:766-772, 814-823`):
            // ignore on `min` users; clear when `stories_unavailable`; update when `stories_max_id` is present.
            if !is_min {
                if d.stories_unavailable {
                    user.flags.remove(UserFlags::HAS_ACTIVE_STORIES);
                    user.flags.remove(UserFlags::HAS_ACTIVE_VIDEO_STREAM);
                } else if let Some(tl_enums::RecentStory::RecentStory(rs)) = &d.stories_max_id {
                    user.flags
                        .set(UserFlags::HAS_ACTIVE_STORIES, rs.max_id.is_some());
                    user.flags.set(UserFlags::HAS_ACTIVE_VIDEO_STREAM, rs.live);
                }
            }

            // Stars per message & linked community.
            if let Some(stars) = d.send_paid_messages_stars {
                user.stars_per_message = stars as i32;
                user.flags.set(UserFlags::HAS_STARS_PER_MESSAGE, stars > 0);
            } else if !is_min {
                user.stars_per_message = 0;
                user.flags.remove(UserFlags::HAS_STARS_PER_MESSAGE);
            }
            if let Some(comm_id) = d.linked_community_id {
                user.linked_community_id = Some(ChannelId::from_tl(comm_id));
            }

            // Bot info (`data/data_session.cpp:714-742`): detailed flags are only carried by
            // non-min users, so a `min` bot must not wipe previously loaded bot flags.
            if d.bot {
                let bot = user.bot_info.get_or_insert_with(BotInfo::default);
                if let Some(ver) = d.bot_info_version {
                    bot.version = ver;
                }
                if !is_min {
                    bot.reads_all_history = d.bot_chat_history;
                    bot.cant_join_groups = d.bot_nochats;
                    bot.supports_attach_menu = d.bot_attach_menu;
                    bot.can_edit_information = d.bot_can_edit;
                    bot.supports_business = d.bot_business;
                    bot.has_main_app = d.bot_has_main_app;
                    bot.user_creates_topics = d.bot_forum_can_manage_topics;
                    bot.can_manage_bots = d.bot_can_manage_bots;
                    bot.inline_placeholder = d.bot_inline_placeholder.clone().unwrap_or_default();
                    bot.active_users = d.bot_active_users.unwrap_or(0);
                }
            } else if !is_min {
                user.bot_info = None;
            }

            // Colors & emoji status (`data/data_session.cpp:919-941`) and restriction reasons
            // (`data/data_session.cpp:824-825`: non-min only).
            if !is_min {
                apply_peer_color(&mut user.common, d.color.as_ref(), false);
                apply_peer_color(&mut user.common, d.profile_color.as_ref(), true);
                user.common.emoji_status = emoji_status_from_tl(d.emoji_status.as_ref());
                user.common
                    .set_unavailable_reasons(restriction_reasons_from_tl(
                        d.restriction_reason.as_deref(),
                    ));
            } else {
                if d.color.is_some() {
                    apply_peer_color(&mut user.common, d.color.as_ref(), false);
                }
                if d.profile_color.is_some() {
                    apply_peer_color(&mut user.common, d.profile_color.as_ref(), true);
                }
                if d.emoji_status.is_some() {
                    user.common.emoji_status = emoji_status_from_tl(d.emoji_status.as_ref());
                }
            }

            if !is_min {
                if user.common.loaded_status < LoadedStatus::Normal {
                    user.common.loaded_status = LoadedStatus::Normal;
                }
            } else if user.common.loaded_status == LoadedStatus::Not {
                user.common.loaded_status = LoadedStatus::Minimal;
            }

            changed |= PeerUpdateFlags::IS_CONTACT | PeerUpdateFlags::IS_BOT;
            session.upsert_user(user, changed);
            Some(peer_id)
        }
    }
}

/// Applies a TL `UserFull` into `session` (updating bio, block/calls status, settings,
/// bot info, birthday, personal channel, verification, rating, and marking `LoadedStatus::Full`).
pub fn apply_tl_user_full(session: &mut Session, tl_full: &tl_enums::UserFull) -> PeerId {
    let tl_enums::UserFull::UserFull(f) = tl_full;
    let uid = UserId::from_tl(f.id);
    let peer_id = PeerId::from_user(uid);
    let mut user = session
        .user(uid)
        .cloned()
        .unwrap_or_else(|| User::new(uid, uid == session.self_user_id()));

    user.common.about = f.about.clone().unwrap_or_default();
    user.common.block_status = if f.blocked {
        BlockStatus::Blocked
    } else {
        BlockStatus::NotBlocked
    };
    user.flags.set(UserFlags::BLOCKED, f.blocked);
    user.flags
        .set(UserFlags::HAS_PHONE_CALLS, f.phone_calls_available);
    user.flags
        .set(UserFlags::PHONE_CALLS_PRIVATE, f.phone_calls_private);
    user.calls_status = if f.phone_calls_private {
        CallsStatus::Private
    } else if f.phone_calls_available {
        CallsStatus::Enabled
    } else {
        CallsStatus::Disabled
    };
    user.flags
        .set(UserFlags::CAN_PIN_MESSAGES, f.can_pin_message);
    user.flags.set(
        UserFlags::VOICE_MESSAGES_FORBIDDEN,
        f.voice_messages_forbidden,
    );
    user.flags.set(
        UserFlags::REQUIRES_PREMIUM_TO_WRITE,
        f.contact_require_premium,
    );
    user.flags
        .set(UserFlags::READ_DATES_PRIVATE, f.read_dates_private);
    user.flags
        .set(UserFlags::NO_FORWARDS_MY_ENABLED, f.noforwards_my_enabled);
    user.flags.set(
        UserFlags::NO_FORWARDS_PEER_ENABLED,
        f.noforwards_peer_enabled,
    );
    user.flags.set(
        UserFlags::UNOFFICIAL_SECURITY_RISK,
        f.unofficial_security_risk,
    );

    user.common.notify = peer_notify_settings_from_tl(&f.notify_settings);
    user.common.bar_settings = peer_bar_settings_from_tl(&f.settings);
    user.common.messages_ttl = f.ttl_period.unwrap_or(0);
    user.common.theme_token = chat_theme_token_from_tl(f.theme.as_ref());
    user.common.main_profile_tab = profile_tab_from_tl(f.main_tab.as_ref());
    user.common_chats_count = f.common_chats_count;
    user.peer_gifts_count = f.stargifts_count.unwrap_or(0);

    if let Some(stars) = f.send_paid_messages_stars {
        user.stars_per_message = stars as i32;
        user.flags.set(UserFlags::HAS_STARS_PER_MESSAGE, stars > 0);
    }
    user.flags
        .insert(UserFlags::MESSAGE_MONEY_RESTRICTIONS_KNOWN);

    user.birthday = f.birthday.as_ref().map(|b| {
        let tl_enums::Birthday::Birthday(bd) = b;
        Birthday {
            day: bd.day.clamp(0, 255) as u8,
            month: bd.month.clamp(0, 255) as u8,
            year: bd.year.unwrap_or(0).clamp(0, u16::MAX as i32) as u16,
        }
    });
    user.personal_channel_id = f.personal_channel_id.map(ChannelId::from_tl);
    user.personal_channel_message_id = MsgId(i64::from(f.personal_channel_message.unwrap_or(0)));
    user.bot_manager_id = f.bot_manager_id.map(UserId::from_tl);
    user.bot_verify_details = f.bot_verification.as_ref().map(bot_verification_from_tl);
    if let Some(tl_enums::StarsRating::StarsRating(r)) = &f.stars_rating {
        user.stars_rating = StarsRating {
            level: r.level,
            stars: r.stars.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
            this_level_stars: r
                .current_level_stars
                .clamp(i64::from(i32::MIN), i64::from(i32::MAX))
                as i32,
            next_level_stars: r
                .next_level_stars
                .unwrap_or(0)
                .clamp(i64::from(i32::MIN), i64::from(i32::MAX))
                as i32,
        };
    }
    if let Some(note) = &f.note {
        user.note = text_with_entities_from_tl(note);
    }

    if let Some(tl_enums::BotInfo::BotInfo(bi)) = &f.bot_info {
        let bot = user.bot_info.get_or_insert_with(BotInfo::default);
        bot.inited = true;
        bot.description = bi.description.clone().unwrap_or_default();
        bot.photo_id = bi.description_photo.as_ref().and_then(photo_id_from_tl);
        bot.document_id = bi.description_document.as_ref().and_then(|doc| match doc {
            tl_enums::Document::Document(d) => Some(DocumentId::from_tl(d.id)),
            tl_enums::Document::Empty(_) => None,
        });
        bot.commands = bi
            .commands
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|c| {
                let tl_enums::BotCommand::BotCommand(bc) = c;
                BotCommand {
                    command: bc.command.clone(),
                    description: bc.description.clone(),
                }
            })
            .collect();
        if let Some(tl_enums::BotMenuButton::BotMenuButton(mb)) = &bi.menu_button {
            bot.menu_button_text = mb.text.clone();
            bot.menu_button_url = mb.url.clone();
        }
        bot.privacy_policy_url = bi.privacy_policy_url.clone().unwrap_or_default();
        if let Some(rights) = &f.bot_group_admin_rights {
            bot.group_admin_rights = admin_rights_from_tl(rights);
        }
        if let Some(rights) = &f.bot_broadcast_admin_rights {
            bot.channel_admin_rights = admin_rights_from_tl(rights);
        }
    }

    // Cache full profile/personal/fallback `PhotoData` (including `access_hash` and
    // `file_reference`) and resolve the active `userpic_photo_id` with tdesktop priority:
    // personal > profile > fallback (`data/data_session.cpp` / `api/api_peer_photo.cpp`).
    let mut active_photo_id: Option<PhotoId> = None;
    let mut explicitly_empty = false;
    for photo in [&f.fallback_photo, &f.profile_photo, &f.personal_photo]
        .into_iter()
        .flatten()
    {
        if let Some(data) = photo_from_tl(photo) {
            active_photo_id = Some(data.id);
            session.cache_photo(data);
        } else {
            explicitly_empty = true;
        }
    }
    if let Some(pid) = active_photo_id {
        user.common.userpic_photo_id = pid;
    } else if explicitly_empty
        || (f.personal_photo.is_none()
            && f.profile_photo.is_none()
            && f.fallback_photo.is_none()
            && user.common.userpic_photo_id == tg_data::UNKNOWN_PHOTO_ID)
    {
        user.common.userpic_photo_id = PhotoId::ZERO;
    }

    user.common.loaded_status = LoadedStatus::Full;
    session.upsert_user(
        user,
        PeerUpdateFlags::FULL_INFO
            | PeerUpdateFlags::ABOUT
            | PeerUpdateFlags::NOTIFICATIONS
            | PeerUpdateFlags::IS_BLOCKED,
    );
    if let Some(folder_id) = f.folder_id {
        session.set_history_folder(peer_id, Some(folder_id));
    }
    peer_id
}

/// Applies a TL `Chat` (`chat`, `chatForbidden`, `chatEmpty`, `channel`, `channelForbidden`,
/// `community`, `communityForbidden`) into `session`, faithfully porting
/// `Data::Session::processChat` (`data_session.cpp`) including `min` merge rules,
/// migration links, and `access_hash` bookkeeping.
pub fn apply_tl_chat(session: &mut Session, tl_chat: &tl_enums::Chat) -> Option<PeerId> {
    match tl_chat {
        tl_enums::Chat::Empty(e) => {
            let cid = ChatId::from_tl(e.id);
            if cid.is_zero() {
                return None;
            }
            let peer_id = PeerId::from_chat(cid);
            let mut chat = session.chat(cid).cloned().unwrap_or_else(|| Chat::new(cid));
            chat.flags.insert(ChatFlags::FORBIDDEN);
            chat.common.loaded_status = LoadedStatus::Normal;
            session.upsert_chat(chat, PeerUpdateFlags::RIGHTS);
            Some(peer_id)
        }
        tl_enums::Chat::Forbidden(f) => {
            let cid = ChatId::from_tl(f.id);
            if cid.is_zero() {
                return None;
            }
            let peer_id = PeerId::from_chat(cid);
            let mut chat = session.chat(cid).cloned().unwrap_or_else(|| Chat::new(cid));
            // Match `Session::processChat(MTPDchatForbidden)` (`data/data_session.cpp:1051-1067`):
            // reset photo, date, count = -1, invalidate participants, replace flags with
            // `FORBIDDEN`, and clear admin rights & default restrictions.
            chat.set_name(&f.title);
            chat.common.userpic_photo_id = PhotoId::ZERO;
            chat.common.userpic_has_video = false;
            chat.date = 0;
            chat.count = -1;
            chat.invalidate_participants();
            chat.flags = ChatFlags::FORBIDDEN;
            chat.admin_rights = ChatAdminRights::empty();
            chat.default_restrictions = ChatBannedRights::empty();
            chat.common.loaded_status = LoadedStatus::Normal;
            session.upsert_chat(
                chat,
                PeerUpdateFlags::NAME | PeerUpdateFlags::PHOTO | PeerUpdateFlags::RIGHTS,
            );
            Some(peer_id)
        }
        tl_enums::Chat::Chat(c) => {
            let cid = ChatId::from_tl(c.id);
            if cid.is_zero() {
                return None;
            }
            let peer_id = PeerId::from_chat(cid);
            let mut chat = session.chat(cid).cloned().unwrap_or_else(|| Chat::new(cid));

            // Match `Session::processChat(MTPDchat)` (`data/data_session.cpp:992-995`):
            // invalidate cached participants when the server chat version advances.
            if chat.version < c.version {
                chat.version = c.version;
                chat.invalidate_participants();
            }
            chat.set_name(&c.title);
            chat.count = c.participants_count;
            chat.date = c.date;
            chat.flags.remove(ChatFlags::FORBIDDEN);
            chat.flags.set(ChatFlags::CREATOR, c.creator);
            chat.flags.set(ChatFlags::LEFT, c.left);
            chat.flags.set(ChatFlags::DEACTIVATED, c.deactivated);
            chat.flags.set(ChatFlags::CALL_ACTIVE, c.call_active);
            chat.flags.set(ChatFlags::CALL_NOT_EMPTY, c.call_not_empty);
            chat.flags.set(ChatFlags::NO_FORWARDS, c.noforwards);

            match &c.photo {
                tl_enums::ChatPhoto::ChatPhoto(p) => {
                    chat.common.userpic_photo_id = PhotoId::from_tl(p.photo_id);
                    chat.common.userpic_has_video = p.has_video;
                }
                tl_enums::ChatPhoto::Empty => {
                    chat.common.userpic_photo_id = PhotoId::ZERO;
                    chat.common.userpic_has_video = false;
                }
            }
            chat.admin_rights = c
                .admin_rights
                .as_ref()
                .map(admin_rights_from_tl)
                .unwrap_or_else(ChatAdminRights::empty);
            chat.default_restrictions = c
                .default_banned_rights
                .as_ref()
                .map(|r| banned_rights_from_tl(r).flags)
                .unwrap_or_else(ChatBannedRights::empty);
            if chat.common.loaded_status < LoadedStatus::Normal {
                chat.common.loaded_status = LoadedStatus::Normal;
            }

            let migrated = match &c.migrated_to {
                Some(tl_enums::InputChannel::InputChannel(ic)) => {
                    let target_id = ChannelId::from_tl(ic.channel_id);
                    chat.migrated_to = Some(target_id);
                    Some((target_id, ic.access_hash))
                }
                _ => None,
            };

            session.upsert_chat(
                chat,
                PeerUpdateFlags::NAME
                    | PeerUpdateFlags::PHOTO
                    | PeerUpdateFlags::MEMBERS
                    | PeerUpdateFlags::RIGHTS,
            );
            if let Some((target_id, access_hash)) = migrated {
                let target_peer = PeerId::from_channel(target_id);
                // Match `Session::processChat` (`data/data_session.cpp:1016-1018`):
                // only populate target channel access_hash if none is known yet.
                if access_hash != 0 && session.peer_access_hash(target_peer).is_none_or(|h| h == 0)
                {
                    session.set_peer_access_hash(target_peer, access_hash);
                }
                session.apply_migration(cid, target_id);
            }
            Some(peer_id)
        }
        tl_enums::Chat::ChannelForbidden(f) => {
            let chid = ChannelId::from_tl(f.id);
            if chid.is_zero() {
                return None;
            }
            let peer_id = PeerId::from_channel(chid);
            let mut ch = session
                .channel(chid)
                .cloned()
                .unwrap_or_else(|| Channel::new(chid));
            // Match `Session::processChat(MTPDchannelForbidden)`
            // (`data/data_session.cpp:1266-1308`): mask-update only Broadcast/Megagroup/
            // Monoforum/Forbidden, clear admin rights, restrictions (unless `until_date` is set),
            // photo, date, and members count.
            ch.access_hash = f.access_hash as u64;
            session.set_peer_access_hash(peer_id, f.access_hash);
            ch.set_name(&f.title, "");
            let flags_mask = ChannelFlags::BROADCAST
                | ChannelFlags::MEGAGROUP
                | ChannelFlags::FORBIDDEN
                | ChannelFlags::MONOFORUM;
            let mut flags_set = ChannelFlags::FORBIDDEN;
            flags_set.set(ChannelFlags::BROADCAST, f.broadcast);
            flags_set.set(ChannelFlags::MEGAGROUP, f.megagroup);
            flags_set.set(ChannelFlags::MONOFORUM, f.monoforum);
            ch.set_flags((ch.flags & !flags_mask) | flags_set);
            ch.admin_rights = ChatAdminRights::empty();
            ch.restrictions = match f.until_date {
                Some(until) => ChatRestrictionsInfo::new(ChatBannedRights::VIEW_MESSAGES, until),
                None => ChatRestrictionsInfo::default(),
            };
            ch.common.userpic_photo_id = PhotoId::ZERO;
            ch.common.userpic_has_video = false;
            ch.date = 0;
            ch.members_count = 0;
            ch.common.loaded_status = LoadedStatus::Normal;
            session.upsert_channel(
                ch,
                PeerUpdateFlags::NAME
                    | PeerUpdateFlags::PHOTO
                    | PeerUpdateFlags::CHANNEL_AM_IN
                    | PeerUpdateFlags::RIGHTS,
            );
            Some(peer_id)
        }
        tl_enums::Chat::Channel(d) => {
            let chid = ChannelId::from_tl(d.id);
            if chid.is_zero() {
                return None;
            }
            let peer_id = PeerId::from_channel(chid);
            let mut ch = session
                .channel(chid)
                .cloned()
                .unwrap_or_else(|| Channel::new(chid));

            let is_min = d.min;

            if !is_min {
                if let Some(hash) = d.access_hash {
                    ch.access_hash = hash as u64;
                    session.set_peer_access_hash(peer_id, hash);
                }
            } else if ch.access_hash == 0
                && let Some(hash) = d.access_hash
                && hash != 0
            {
                ch.access_hash = hash as u64;
                session.set_peer_access_hash(peer_id, hash);
            }

            let editable_username = d.username.as_deref().unwrap_or("");
            ch.set_name(&d.title, editable_username);
            // Match `Session::processChat(MTPDchannel)` (`data/data_session.cpp:1127-1134`):
            // always sync active usernames list so removed collectible usernames are cleared.
            if let Some(list) = &d.usernames {
                ch.set_usernames(&usernames_from_tl(list));
            } else if !editable_username.is_empty() {
                ch.set_usernames(&[Username {
                    username: editable_username.to_owned(),
                    active: true,
                    editable: true,
                }]);
            } else {
                ch.set_usernames(&[]);
            }

            // `default_banned_rights` and `subscription_until_date` are carried on both min and
            // full channels (`data/data_session.cpp:1090-1099`).
            ch.default_restrictions = d
                .default_banned_rights
                .as_ref()
                .map(|r| banned_rights_from_tl(r).flags)
                .unwrap_or_else(ChatBannedRights::empty);
            ch.subscription_until_date = d.subscription_until_date.unwrap_or(0);

            match &d.photo {
                tl_enums::ChatPhoto::ChatPhoto(p) => {
                    ch.common.userpic_photo_id = PhotoId::from_tl(p.photo_id);
                    ch.common.userpic_has_video = p.has_video;
                }
                tl_enums::ChatPhoto::Empty => {
                    ch.common.userpic_photo_id = PhotoId::ZERO;
                    ch.common.userpic_has_video = false;
                }
            }

            let mut flags = ch.flags;
            flags.remove(ChannelFlags::FORBIDDEN);
            flags.set(ChannelFlags::BROADCAST, d.broadcast);
            flags.set(ChannelFlags::MEGAGROUP, d.megagroup);
            flags.set(ChannelFlags::GIGAGROUP, d.gigagroup);
            flags.set(ChannelFlags::VERIFIED, d.verified);
            flags.set(ChannelFlags::SIGNATURES, d.signatures);
            flags.set(ChannelFlags::SIGNATURE_PROFILES, d.signature_profiles);
            flags.set(ChannelFlags::SCAM, d.scam);
            flags.set(ChannelFlags::FAKE, d.fake);
            flags.set(ChannelFlags::HAS_LINK, d.has_link);
            flags.set(ChannelFlags::LOCATION, d.has_geo);
            flags.set(ChannelFlags::SLOWMODE_ENABLED, d.slowmode_enabled);
            flags.set(ChannelFlags::CALL_ACTIVE, d.call_active);
            flags.set(ChannelFlags::CALL_NOT_EMPTY, d.call_not_empty);
            flags.set(ChannelFlags::NO_FORWARDS, d.noforwards);
            flags.set(ChannelFlags::JOIN_TO_WRITE, d.join_to_send);
            flags.set(ChannelFlags::REQUEST_TO_JOIN, d.join_request);
            // Match `Data::Session::processChat` (`data/data_session.cpp:1209-1211`):
            // `FORUM` flag only applies to megagroups.
            flags.set(ChannelFlags::FORUM, d.forum && d.megagroup);
            flags.set(ChannelFlags::FORUM_TABS, d.forum_tabs);
            flags.set(ChannelFlags::AUTO_TRANSLATION, d.autotranslation);
            flags.set(ChannelFlags::MONOFORUM, d.monoforum);
            // Match `Data::Session::processChat` + `ChannelData::setMonoforumLink`
            // (`data/data_session.cpp:1219,1240-1242`, `data/data_channel.cpp:369-390`):
            // `MONOFORUM_DISABLED` applies only to monoforums when unlinked from their broadcast
            // channel, never from a regular channel's `!broadcast_messages_allowed`.
            if !d.monoforum {
                flags.remove(ChannelFlags::MONOFORUM_DISABLED);
            }
            ch.broadcast_messages_allowed = d.broadcast_messages_allowed;

            if !is_min {
                // `date`, `creator`, `left`, `admin_rights`, `banned_rights`, `stories_hidden`
                // (when `!stories_hidden_min`), `stories_max_id`, and `restriction_reason` are
                // only updated from non-min channels (`data/data_session.cpp:1106-1237`).
                ch.date = d.date;
                flags.set(ChannelFlags::CREATOR, d.creator);
                flags.set(ChannelFlags::LEFT, d.left);
                // Match `Data::Session::processChat` (`data/data_session.cpp:1161, 1213-1217`):
                // do not overwrite `STORIES_HIDDEN` when `stories_hidden_min` is set.
                if !d.stories_hidden_min {
                    flags.set(ChannelFlags::STORIES_HIDDEN, d.stories_hidden);
                }
                ch.admin_rights = d
                    .admin_rights
                    .as_ref()
                    .map(admin_rights_from_tl)
                    .unwrap_or_else(ChatAdminRights::empty);
                ch.restrictions = d
                    .banned_rights
                    .as_ref()
                    .map(banned_rights_from_tl)
                    .unwrap_or_default();
                ch.default_restrictions = d
                    .default_banned_rights
                    .as_ref()
                    .map(|r| banned_rights_from_tl(r).flags)
                    .unwrap_or_else(ChatBannedRights::empty);
                ch.subscription_until_date = d.subscription_until_date.unwrap_or(0);
                // Match `Data::Session::processChat` (`data/data_session.cpp:1178-1184`):
                // ignore on `min`; clear when `stories_unavailable`; update when `stories_max_id` is present.
                if d.stories_unavailable {
                    flags.remove(ChannelFlags::HAS_ACTIVE_STORIES);
                    flags.remove(ChannelFlags::HAS_ACTIVE_VIDEO_STREAM);
                } else if let Some(tl_enums::RecentStory::RecentStory(rs)) = &d.stories_max_id {
                    flags.set(ChannelFlags::HAS_ACTIVE_STORIES, rs.max_id.is_some());
                    flags.set(ChannelFlags::HAS_ACTIVE_VIDEO_STREAM, rs.live);
                }
                ch.common
                    .set_unavailable_reasons(restriction_reasons_from_tl(
                        d.restriction_reason.as_deref(),
                    ));
            }
            if let Some(count) = d.participants_count {
                ch.members_count = count;
            }
            if let Some(level) = d.level {
                ch.level_hint = level;
            }
            if let Some(stars) = d.send_paid_messages_stars {
                ch.stars_per_message = stars as i32;
                flags.set(ChannelFlags::HAS_STARS_PER_MESSAGE, stars > 0);
            } else if !is_min {
                ch.stars_per_message = 0;
                flags.remove(ChannelFlags::HAS_STARS_PER_MESSAGE);
            }
            let prev_monoforum_link = ch.monoforum_link;
            let linked_monoforum_id = d
                .linked_monoforum_id
                .filter(|&id| id != 0)
                .map(ChannelId::from_tl);
            if let Some(mono) = linked_monoforum_id {
                ch.monoforum_link = Some(mono);
                if d.monoforum {
                    flags.remove(ChannelFlags::MONOFORUM_DISABLED);
                }
            } else if d.monoforum && prev_monoforum_link.is_some() {
                flags.insert(ChannelFlags::MONOFORUM_DISABLED);
            }
            if let Some(comm) = d.linked_community_id {
                ch.linked_community_id = Some(ChannelId::from_tl(comm));
            }

            ch.set_flags(flags);

            if !is_min {
                apply_peer_color(&mut ch.common, d.color.as_ref(), false);
                apply_peer_color(&mut ch.common, d.profile_color.as_ref(), true);
                ch.common.emoji_status = emoji_status_from_tl(d.emoji_status.as_ref());
            } else {
                if d.color.is_some() {
                    apply_peer_color(&mut ch.common, d.color.as_ref(), false);
                }
                if d.profile_color.is_some() {
                    apply_peer_color(&mut ch.common, d.profile_color.as_ref(), true);
                }
                if d.emoji_status.is_some() {
                    ch.common.emoji_status = emoji_status_from_tl(d.emoji_status.as_ref());
                }
            }

            if !is_min {
                if ch.common.loaded_status < LoadedStatus::Normal {
                    ch.common.loaded_status = LoadedStatus::Normal;
                }
            } else if ch.common.loaded_status == LoadedStatus::Not {
                ch.common.loaded_status = LoadedStatus::Minimal;
            }

            session.upsert_channel(
                ch,
                PeerUpdateFlags::NAME
                    | PeerUpdateFlags::USERNAME
                    | PeerUpdateFlags::PHOTO
                    | PeerUpdateFlags::CHANNEL_AM_IN
                    | PeerUpdateFlags::MEMBERS
                    | PeerUpdateFlags::RIGHTS,
            );
            // Mirror `ChannelData::setMonoforumLink` (`data/data_channel.cpp:369-390`):
            // linking/unlinking a broadcast channel updates `MONOFORUM_DISABLED` on the linked
            // monoforum channel only.
            if d.broadcast {
                if let Some(mono_id) = linked_monoforum_id {
                    if let Some(tg_data::Peer::Channel(mono)) =
                        session.peer_mut(PeerId::from_channel(mono_id))
                        && mono.is_monoforum()
                    {
                        mono.monoforum_link = Some(chid);
                        mono.flags.remove(ChannelFlags::MONOFORUM_DISABLED);
                    }
                } else if let Some(old_mono_id) = prev_monoforum_link
                    && let Some(tg_data::Peer::Channel(mono)) =
                        session.peer_mut(PeerId::from_channel(old_mono_id))
                    && mono.is_monoforum()
                {
                    mono.flags.insert(ChannelFlags::MONOFORUM_DISABLED);
                }
            } else if d.monoforum
                && let Some(bc_id) = linked_monoforum_id
                && let Some(tg_data::Peer::Channel(bc)) =
                    session.peer_mut(PeerId::from_channel(bc_id))
                && bc.is_broadcast()
            {
                bc.monoforum_link = Some(chid);
            }
            Some(peer_id)
        }
        tl_enums::Chat::Community(c) => {
            let chid = ChannelId::from_tl(c.id);
            if chid.is_zero() {
                return None;
            }
            let peer_id = PeerId::from_channel(chid);
            let mut ch = session
                .channel(chid)
                .cloned()
                .unwrap_or_else(|| Channel::new(chid));
            if let Some(hash) = c.access_hash
                && (!c.min || ch.access_hash == 0)
            {
                ch.access_hash = hash as u64;
                session.set_peer_access_hash(peer_id, hash);
            }
            ch.set_name(&c.title, "");
            ch.date = c.date;
            let mut flags = ch.flags;
            flags.remove(ChannelFlags::FORBIDDEN);
            flags.insert(ChannelFlags::COMMUNITY);
            flags.set(ChannelFlags::COMMUNITY_COLLAPSED, c.collapsed_in_dialogs);
            if !c.min {
                flags.set(ChannelFlags::CREATOR, c.creator);
                flags.set(ChannelFlags::LEFT, c.left);
                ch.admin_rights = c
                    .admin_rights
                    .as_ref()
                    .map(admin_rights_from_tl)
                    .unwrap_or_else(ChatAdminRights::empty);
                ch.default_restrictions = c
                    .default_banned_rights
                    .as_ref()
                    .map(|r| banned_rights_from_tl(r).flags)
                    .unwrap_or_else(ChatBannedRights::empty);
            }
            match &c.photo {
                tl_enums::ChatPhoto::ChatPhoto(p) => {
                    ch.common.userpic_photo_id = PhotoId::from_tl(p.photo_id);
                    ch.common.userpic_has_video = p.has_video;
                }
                tl_enums::ChatPhoto::Empty => {
                    ch.common.userpic_photo_id = PhotoId::ZERO;
                    ch.common.userpic_has_video = false;
                }
            }
            ch.set_flags(flags);
            if ch.common.loaded_status < LoadedStatus::Normal {
                ch.common.loaded_status = if c.min {
                    LoadedStatus::Minimal
                } else {
                    LoadedStatus::Normal
                };
            }
            session.upsert_channel(
                ch,
                PeerUpdateFlags::NAME | PeerUpdateFlags::CHANNEL_AM_IN | PeerUpdateFlags::RIGHTS,
            );
            Some(peer_id)
        }
        tl_enums::Chat::CommunityForbidden(f) => {
            let chid = ChannelId::from_tl(f.id);
            if chid.is_zero() {
                return None;
            }
            let peer_id = PeerId::from_channel(chid);
            let mut ch = session
                .channel(chid)
                .cloned()
                .unwrap_or_else(|| Channel::new(chid));
            if let Some(hash) = f.access_hash {
                ch.access_hash = hash as u64;
                session.set_peer_access_hash(peer_id, hash);
            }
            ch.set_name(&f.title, "");
            ch.set_flags(ChannelFlags::FORBIDDEN | ChannelFlags::COMMUNITY);
            ch.common.loaded_status = LoadedStatus::Normal;
            session.upsert_channel(
                ch,
                PeerUpdateFlags::NAME | PeerUpdateFlags::CHANNEL_AM_IN | PeerUpdateFlags::RIGHTS,
            );
            Some(peer_id)
        }
    }
}

/// Applies a TL `ChatFull` (`chatFull`, `channelFull`, or `communityFull`) into `session`.
pub fn apply_tl_chat_full(session: &mut Session, tl_full: &tl_enums::ChatFull) -> PeerId {
    match tl_full {
        tl_enums::ChatFull::ChatFull(f) => {
            let cid = ChatId::from_tl(f.id);
            let peer_id = PeerId::from_chat(cid);
            let mut chat = session.chat(cid).cloned().unwrap_or_else(|| Chat::new(cid));

            chat.common.about = f.about.clone();
            chat.common.notify = peer_notify_settings_from_tl(&f.notify_settings);
            chat.common.messages_ttl = f.ttl_period.unwrap_or(0);
            chat.common.theme_token = f.theme_emoticon.clone().unwrap_or_default();
            chat.flags
                .set(ChatFlags::CAN_SET_USERNAME, f.can_set_username);
            chat.flags
                .set(ChatFlags::HAS_WELCOME_MESSAGES, f.has_welcome_messages);
            chat.pending_requests_count = f.requests_pending.unwrap_or(0);
            chat.recent_requesters = f
                .recent_requesters
                .as_deref()
                .unwrap_or_default()
                .iter()
                .copied()
                .map(UserId::from_tl)
                .collect();
            chat.allowed_reactions =
                allowed_reactions_from_tl(f.available_reactions.as_ref(), f.reactions_limit, false);
            if let Some(tl_enums::ExportedChatInvite::ChatInviteExported(inv)) = &f.exported_invite
            {
                chat.invite_link = inv.link.clone();
            }
            match &f.call {
                Some(tl_enums::InputGroupCall::InputGroupCall(gc)) => {
                    chat.group_call = Some((CallId::from_tl(gc.id), gc.access_hash as u64));
                    chat.flags.insert(ChatFlags::CALL_ACTIVE);
                }
                _ => {
                    chat.group_call = None;
                    chat.flags
                        .remove(ChatFlags::CALL_ACTIVE | ChatFlags::CALL_NOT_EMPTY);
                }
            }

            match &f.participants {
                tl_enums::ChatParticipants::ChatParticipants(ps) => {
                    chat.version = ps.version;
                    chat.participants.clear();
                    chat.admins.clear();
                    chat.member_ranks.clear();
                    for p in &ps.participants {
                        match p {
                            tl_enums::ChatParticipant::ChatParticipant(m) => {
                                let uid = UserId::from_tl(m.user_id);
                                chat.participants.insert(uid);
                                if let Some(rank) = &m.rank {
                                    chat.member_ranks.insert(uid, rank.clone());
                                }
                            }
                            tl_enums::ChatParticipant::Creator(m) => {
                                let uid = UserId::from_tl(m.user_id);
                                chat.creator = uid;
                                chat.participants.insert(uid);
                                if let Some(rank) = &m.rank {
                                    chat.member_ranks.insert(uid, rank.clone());
                                }
                            }
                            tl_enums::ChatParticipant::Admin(m) => {
                                let uid = UserId::from_tl(m.user_id);
                                chat.participants.insert(uid);
                                chat.admins.insert(uid);
                                if let Some(rank) = &m.rank {
                                    chat.member_ranks.insert(uid, rank.clone());
                                }
                            }
                        }
                    }
                    chat.count = chat.participants.len() as i32;
                }
                tl_enums::ChatParticipants::Forbidden(_) => {}
            }

            if let Some(photo) = &f.chat_photo {
                if let Some(data) = photo_from_tl(photo) {
                    chat.common.userpic_photo_id = data.id;
                    session.cache_photo(data);
                } else {
                    chat.common.userpic_photo_id = PhotoId::ZERO;
                }
            }
            chat.common.loaded_status = LoadedStatus::Full;
            session.upsert_chat(
                chat,
                PeerUpdateFlags::FULL_INFO
                    | PeerUpdateFlags::ABOUT
                    | PeerUpdateFlags::NOTIFICATIONS
                    | PeerUpdateFlags::MEMBERS,
            );
            if let Some(folder_id) = f.folder_id {
                session.set_history_folder(peer_id, Some(folder_id));
            }
            peer_id
        }
        tl_enums::ChatFull::ChannelFull(f) => {
            let chid = ChannelId::from_tl(f.id);
            let peer_id = PeerId::from_channel(chid);
            let mut ch = session
                .channel(chid)
                .cloned()
                .unwrap_or_else(|| Channel::new(chid));

            ch.common.about = f.about.clone();
            ch.common.notify = peer_notify_settings_from_tl(&f.notify_settings);
            ch.common.messages_ttl = f.ttl_period.unwrap_or(0);
            ch.common.theme_token = f.theme_emoticon.clone().unwrap_or_default();
            ch.common.main_profile_tab = profile_tab_from_tl(f.main_tab.as_ref());
            ch.common.block_status = if f.blocked {
                BlockStatus::Blocked
            } else {
                BlockStatus::NotBlocked
            };

            let mut flags = ch.flags;
            flags.set(ChannelFlags::CAN_VIEW_PARTICIPANTS, f.can_view_participants);
            flags.set(ChannelFlags::CAN_SET_USERNAME, f.can_set_username);
            flags.set(ChannelFlags::CAN_SET_STICKERS, f.can_set_stickers);
            flags.set(ChannelFlags::PRE_HISTORY_HIDDEN, f.hidden_prehistory);
            flags.set(ChannelFlags::CAN_GET_STATISTICS, f.can_view_stats);
            flags.set(ChannelFlags::ANTI_SPAM, f.antispam);
            flags.set(ChannelFlags::PARTICIPANTS_HIDDEN, f.participants_hidden);
            flags.set(ChannelFlags::VIEW_AS_MESSAGES, f.view_forum_as_messages);
            flags.set(ChannelFlags::CAN_VIEW_REVENUE, f.can_view_revenue);
            flags.set(ChannelFlags::PAID_MEDIA_ALLOWED, f.paid_media_allowed);
            flags.set(
                ChannelFlags::CAN_VIEW_CREDITS_REVENUE,
                f.can_view_stars_revenue,
            );
            flags.set(ChannelFlags::STARGIFTS_AVAILABLE, f.stargifts_available);
            flags.set(
                ChannelFlags::PAID_MESSAGES_AVAILABLE,
                f.paid_messages_available,
            );
            flags.set(ChannelFlags::HAS_WELCOME_MESSAGES, f.has_welcome_messages);
            flags.insert(ChannelFlags::STARS_PER_MESSAGE_KNOWN);

            if let Some(stars) = f.send_paid_messages_stars {
                ch.stars_per_message = stars as i32;
                flags.set(ChannelFlags::HAS_STARS_PER_MESSAGE, stars > 0);
            }
            match &f.call {
                Some(tl_enums::InputGroupCall::InputGroupCall(gc)) => {
                    ch.group_call = Some((CallId::from_tl(gc.id), gc.access_hash as u64));
                    flags.insert(ChannelFlags::CALL_ACTIVE);
                }
                _ => {
                    ch.group_call = None;
                    flags.remove(ChannelFlags::CALL_ACTIVE | ChannelFlags::CALL_NOT_EMPTY);
                }
            }
            ch.set_flags(flags);

            if let Some(count) = f.participants_count {
                ch.members_count = count;
            }
            if let Some(count) = f.admins_count {
                ch.admins_count = count;
            }
            if let Some(count) = f.kicked_count {
                ch.kicked_count = count;
            }
            if let Some(count) = f.banned_count {
                ch.restricted_count = count;
            }
            ch.pending_requests_count = f.requests_pending.unwrap_or(0);
            ch.recent_requesters = f
                .recent_requesters
                .as_deref()
                .unwrap_or_default()
                .iter()
                .copied()
                .map(UserId::from_tl)
                .collect();
            ch.available_min_id = MsgId(i64::from(f.available_min_id.unwrap_or(0)));
            ch.peer_gifts_count = f.stargifts_count.unwrap_or(0);
            ch.discussion_link = f.linked_chat_id.map(ChannelId::from_tl);
            ch.discussion_link_known = true;
            ch.allowed_reactions = allowed_reactions_from_tl(
                f.available_reactions.as_ref(),
                f.reactions_limit,
                f.paid_reactions_available,
            );
            ch.bot_verify_details = f.bot_verification.as_ref().map(bot_verification_from_tl);
            if let Some(tl_enums::ExportedChatInvite::ChatInviteExported(inv)) = &f.exported_invite
            {
                ch.invite_link = inv.link.clone();
            }
            if let Some(data) = photo_from_tl(&f.chat_photo) {
                ch.common.userpic_photo_id = data.id;
                session.cache_photo(data);
            } else {
                ch.common.userpic_photo_id = PhotoId::ZERO;
            }

            let migrated_from = f.migrated_from_chat_id.map(ChatId::from_tl);
            if ch.is_megagroup()
                || ch.mg_info.is_some()
                || migrated_from.is_some()
                || f.location.is_some()
                || f.slowmode_seconds.is_some()
                || f.boosts_applied.is_some()
                || f.boosts_unrestrict.is_some()
            {
                let mg = ch.ensure_mg_info();
                if let Some(chat_id) = migrated_from {
                    mg.migrated_from = Some(chat_id);
                }
                if let Some(tl_enums::ChannelLocation::ChannelLocation(loc)) = &f.location {
                    mg.location = Some(ChannelLocation {
                        address: loc.address.clone(),
                        point: geo_point_from_tl(&loc.geo_point).unwrap_or_default(),
                    });
                } else if matches!(f.location, Some(tl_enums::ChannelLocation::Empty)) {
                    mg.location = None;
                }
                // Match `Data::Session::processFullPeer` (`data/data_session.cpp`):
                // reset omitted optional ints to `0` so disabling slowmode/boosts does not leave stale values.
                mg.slowmode_seconds = f.slowmode_seconds.unwrap_or(0);
                mg.boosts_applied = f.boosts_applied.unwrap_or(0);
                mg.boosts_unrestrict = f.boosts_unrestrict.unwrap_or(0);
            }

            ch.common.loaded_status = LoadedStatus::Full;
            session.upsert_channel(
                ch,
                PeerUpdateFlags::FULL_INFO
                    | PeerUpdateFlags::ABOUT
                    | PeerUpdateFlags::NOTIFICATIONS
                    | PeerUpdateFlags::MEMBERS,
            );
            if let Some(chat_id) = migrated_from {
                session.apply_migration(chat_id, chid);
            }
            if let Some(folder_id) = f.folder_id {
                session.set_history_folder(peer_id, Some(folder_id));
            }
            peer_id
        }
        tl_enums::ChatFull::CommunityFull(f) => {
            let chid = ChannelId::from_tl(f.id);
            let peer_id = PeerId::from_channel(chid);
            let mut ch = session
                .channel(chid)
                .cloned()
                .unwrap_or_else(|| Channel::new(chid));
            ch.common.about = f.about.clone();
            ch.flags.insert(ChannelFlags::COMMUNITY);
            if let Some(admins) = f.admins_count {
                ch.admins_count = admins;
            }
            if let Some(kicked) = f.kicked_count {
                ch.kicked_count = kicked;
            }
            if let Some(data) = photo_from_tl(&f.chat_photo) {
                ch.common.userpic_photo_id = data.id;
                session.cache_photo(data);
            } else {
                ch.common.userpic_photo_id = PhotoId::ZERO;
            }
            ch.common.loaded_status = LoadedStatus::Full;
            session.upsert_channel(ch, PeerUpdateFlags::FULL_INFO | PeerUpdateFlags::ABOUT);
            peer_id
        }
    }
}

/// Applies slices of TL `User` and `Chat` objects into `session` (`processUsers` / `processChats`).
pub fn apply_tl_users_and_chats(
    session: &mut Session,
    users: &[tl_enums::User],
    chats: &[tl_enums::Chat],
) {
    for user in users {
        apply_tl_user(session, user);
    }
    for chat in chats {
        apply_tl_chat(session, chat);
    }
}
