//! What a group message shows on its sender name line: the rank badge
//! (`Message::refreshRightBadge`) and the premium star or emoji status
//! (`Message::validateFromNameText`, `Message::fromNameStatusId`), plus the request that loads
//! a supergroup's admins and their ranks when it is opened (`ChatParticipants::requestAdmins`,
//! sent by `HistoryWidget::showHistory` while `adminsLoaded` is false).

use tg_app_core::history::backend::{SenderBadge, SenderBadgeRole, SenderNameStatus};
use tg_data::{Message, PeerId, Session};

/// `Message::refreshRightBadge` for an incoming message (`hasOutLayout()` messages get none).
#[must_use]
pub fn sender_badge(session: &Session, msg: &Message, out_layout: bool) -> Option<SenderBadge> {
    if out_layout {
        return None;
    }
    let author = msg.from_id.unwrap_or(msg.peer_id);
    let badge =
        |text: String, role| (!text.trim().is_empty()).then_some(SenderBadge { text, role });
    let in_megagroup = session
        .peer(msg.peer_id)
        .is_some_and(tg_data::Peer::is_megagroup);
    // A channel post copied into its discussion group (`HistoryItem::isDiscussionPost`).
    if in_megagroup
        && msg
            .forward_from
            .as_ref()
            .and_then(|f| f.saved_from_peer)
            .is_some_and(|p| p.is_channel())
    {
        return badge(tg_lang::tr::lng_channel_badge(), SenderBadgeRole::User);
    }
    // An anonymous admin posts as the supergroup; the signature is the admin's rank.
    if session
        .peer(author)
        .is_some_and(tg_data::Peer::is_megagroup)
    {
        return msg
            .post_author
            .clone()
            .and_then(|a| badge(a, SenderBadgeRole::User));
    }
    let user = author.to_user()?;
    if let Some(chat_id) = msg.peer_id.to_chat() {
        let chat = session.chat(chat_id)?;
        let rank = chat.member_ranks.get(&user)?;
        let role = if chat.creator == user {
            SenderBadgeRole::Creator
        } else if chat.admins.contains(&user) {
            SenderBadgeRole::Admin
        } else {
            SenderBadgeRole::User
        };
        return badge(rank.clone(), role);
    }
    let channel = session.channel(msg.peer_id.to_channel()?)?;
    let mg = channel.mg_info.as_ref()?;
    let is_creator = mg.creator == Some(user);
    if is_creator || mg.admins.contains(&user) {
        let role = if is_creator {
            SenderBadgeRole::Creator
        } else {
            SenderBadgeRole::Admin
        };
        if let Some(rank) = mg.member_ranks.get(&user).filter(|r| !r.is_empty()) {
            return badge(rank.clone(), role);
        }
        let text = if is_creator {
            tg_lang::tr::lng_owner_badge()
        } else {
            tg_lang::tr::lng_admin_badge()
        };
        return badge(text, role);
    }
    msg.from_rank
        .clone()
        .and_then(|r| badge(r, SenderBadgeRole::User))
}

/// The star or emoji status after the sender name: shown for Premium users, and for channels
/// posting in another chat when they have an emoji status (`validateFromNameText`).
#[must_use]
pub fn sender_name_status(
    session: &Session,
    author: PeerId,
    history_peer: PeerId,
    now: i32,
) -> Option<SenderNameStatus> {
    let peer = session.peer(author)?;
    let status = &peer.common().emoji_status;
    let status_id = (!status.document_id.is_zero() && (status.until == 0 || status.until > now))
        .then_some(status.document_id.0);
    let premium = peer.is_premium();
    let channel_status =
        peer.as_channel().is_some() && status_id.is_some() && author != history_peer;
    (premium || channel_status).then_some(SenderNameStatus {
        emoji_status_id: status_id,
        premium,
    })
}

/// `true` for a supergroup whose admin list `HistoryWidget::showHistory` would request
/// (`mgInfo->adminsLoaded` is tracked by the once-per-chat open prefetch).
pub(crate) fn group_admins_needed(live: &crate::session::LiveSession, peer: PeerId) -> bool {
    live.read_session(|s| s.peer(peer).is_some_and(tg_data::Peer::is_megagroup))
}

/// `ChatParticipants::requestAdmins`: stores the owner, the admins and their ranks in
/// `MegagroupInfo` (through `load_channel_participants`), then marks the loaded messages of
/// those admins as changed so the open history maps them again with their badges
/// (`Message::applyGroupAdminChanges`). The caller wakes the UI.
pub(crate) fn load_group_admins_now(live: &crate::session::LiveSession, peer: PeerId) {
    if live
        .load_channel_participants(
            crate::convert::from_data_peer(peer),
            crate::search_and_info::ParticipantFilter::Admins,
            0,
            crate::search_and_info::PARTICIPANTS_PER_PAGE,
            0,
        )
        .is_err()
    {
        return;
    }
    let changed = live.read_session(|s| admin_message_ids(s, peer));
    for id in changed {
        live.bridge_state().record_message_edited(peer, id);
    }
}

/// Loaded messages of `peer` written by its admins or owner.
fn admin_message_ids(session: &Session, peer: PeerId) -> Vec<tg_data::MsgId> {
    let Some(mg) = peer
        .to_channel()
        .and_then(|id| session.channel(id))
        .and_then(|ch| ch.mg_info.as_ref())
    else {
        return Vec::new();
    };
    let Some(history) = session.history(peer) else {
        return Vec::new();
    };
    history
        .loaded_ranges
        .slices()
        .iter()
        .flat_map(|slice| slice.messages.iter().copied())
        .filter(|&id| {
            session
                .message(tg_data::FullMsgId::new(peer, id))
                .and_then(|m| m.from_id)
                .and_then(PeerId::to_user)
                .is_some_and(|u| mg.admins.contains(&u) || mg.creator == Some(u))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tg_data::changes::PeerUpdateFlags;
    use tg_data::{Channel, ChannelId, Chat, ChatId, MegagroupInfo, MsgId, User, UserId};

    fn session() -> Session {
        Session::new(UserId(99))
    }

    fn group_message(peer: PeerId, from: UserId) -> Message {
        Message::new_text(
            peer,
            MsgId(10),
            Some(PeerId::from_user(from)),
            1_700_000_000,
            tg_data::TextWithEntities::default(),
        )
    }

    fn megagroup(session: &mut Session, id: ChannelId, mg: MegagroupInfo) -> PeerId {
        let mut ch = Channel::new(id);
        ch.flags.insert(tg_data::ChannelFlags::MEGAGROUP);
        ch.flags.remove(tg_data::ChannelFlags::FORBIDDEN);
        ch.mg_info = Some(mg);
        session.upsert_channel(ch, PeerUpdateFlags::empty());
        PeerId::from_channel(id)
    }

    #[test]
    fn member_rank_comes_from_the_message_and_admins_from_the_group() {
        let mut session = session();
        let owner = UserId(1);
        let admin = UserId(2);
        let member = UserId(3);
        let mut mg = MegagroupInfo {
            creator: Some(owner),
            ..MegagroupInfo::default()
        };
        mg.admins.insert(owner);
        mg.admins.insert(admin);
        mg.member_ranks.insert(admin, "Оригинал Мудрец".to_owned());
        let peer = megagroup(&mut session, ChannelId(50), mg);

        let mut m = group_message(peer, member);
        m.from_rank = Some("няша".to_owned());
        assert_eq!(
            sender_badge(&session, &m, false),
            Some(SenderBadge {
                text: "няша".to_owned(),
                role: SenderBadgeRole::User
            })
        );
        assert_eq!(
            sender_badge(&session, &m, true),
            None,
            "no badge on own messages"
        );

        let m = group_message(peer, admin);
        assert_eq!(
            sender_badge(&session, &m, false),
            Some(SenderBadge {
                text: "Оригинал Мудрец".to_owned(),
                role: SenderBadgeRole::Admin
            })
        );
        let m = group_message(peer, owner);
        let b = sender_badge(&session, &m, false);
        assert_eq!(b.map(|b| b.role), Some(SenderBadgeRole::Creator));

        let m = group_message(peer, UserId(4));
        assert_eq!(sender_badge(&session, &m, false), None);

        // An automatic copy of a channel post in its discussion group.
        let linked = PeerId::from_channel(ChannelId(51));
        let mut m = group_message(peer, member);
        m.from_id = Some(linked);
        m.forward_from = Some(tg_data::ForwardInfo {
            from_id: Some(linked),
            saved_from_peer: Some(linked),
            ..Default::default()
        });
        assert_eq!(
            sender_badge(&session, &m, false),
            Some(SenderBadge {
                text: tg_lang::tr::lng_channel_badge(),
                role: SenderBadgeRole::User
            })
        );
    }

    #[test]
    fn basic_group_ranks_come_from_the_chat() {
        let mut session = session();
        let mut chat = Chat::new(ChatId(7));
        chat.creator = UserId(1);
        chat.member_ranks.insert(UserId(1), "boss".to_owned());
        session.upsert_chat(chat, PeerUpdateFlags::empty());
        let m = group_message(PeerId::from_chat(ChatId(7)), UserId(1));
        assert_eq!(
            sender_badge(&session, &m, false).map(|b| b.role),
            Some(SenderBadgeRole::Creator)
        );
    }

    #[test]
    fn premium_users_get_a_status_with_their_emoji_status() {
        let mut session = session();
        let mut user = User::new(UserId(9), false);
        user.flags.insert(tg_data::UserFlags::PREMIUM);
        user.common.emoji_status.document_id = tg_data::DocumentId(777);
        session.upsert_user(user, PeerUpdateFlags::empty());
        let author = PeerId::from_user(UserId(9));
        let group = PeerId::from_channel(ChannelId(50));
        assert_eq!(
            sender_name_status(&session, author, group, 100),
            Some(SenderNameStatus {
                emoji_status_id: Some(777),
                premium: true
            })
        );
        session.upsert_user(User::new(UserId(10), false), PeerUpdateFlags::empty());
        assert_eq!(
            sender_name_status(&session, PeerId::from_user(UserId(10)), group, 100),
            None
        );
    }
}
