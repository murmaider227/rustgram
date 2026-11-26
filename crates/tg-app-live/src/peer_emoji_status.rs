//! Which custom emoji status a peer shows next to its name in the chat list and in the profile
//! header. Ports the rules of `Ui::PeerBadge::drawGetWidth` (`ui/unread_badge.cpp`),
//! `Info::Profile::BadgeContentForPeer` / `ShownEmojiStatusIdValue`
//! (`info/profile/info_profile_badge.cpp`, `info_profile_values.cpp`) and
//! `PeerData::hideEmojiStatus` (`data/data_peer.cpp`).

use tg_data::{Peer, PeerBarSettings};

/// The status document id, if one is set and has not expired (`PeerData::emojiStatusId`; an
/// expired status is cleared by `Data::EmojiStatuses` when its `until` passes).
fn current_status_id(peer: &Peer, now: i32) -> Option<u64> {
    let status = &peer.common().emoji_status;
    (!status.document_id.is_zero() && (status.until == 0 || status.until > now))
        .then_some(status.document_id.0)
}

/// `PeerData::hideEmojiStatus`: the action bar offers "Report Spam".
fn hides_emoji_status(peer: &Peer) -> bool {
    let bar = peer.common().bar_settings;
    !bar.contains(PeerBarSettings::UNKNOWN) && bar.contains(PeerBarSettings::REPORT_SPAM)
}

/// `ShownEmojiStatusIdValue`: basic groups never show one, spam-flagged peers hide theirs.
fn shown_status_id(peer: &Peer, now: i32) -> Option<u64> {
    if matches!(peer, Peer::Chat(_)) || hides_emoji_status(peer) {
        return None;
    }
    current_status_id(peer, now)
}

/// The emoji status a chat-list row paints after the name: `PeerBadge` shows it for Premium
/// users and for channels (`peer->isPremium() || peer->isChannel()`). A verified peer's row
/// paints the check instead; the painter decides that from `DialogItem::verified`.
#[must_use]
pub fn row_emoji_status_id(peer: &Peer, now: i32) -> Option<u64> {
    let eligible = match peer {
        Peer::User(u) => u.is_premium(),
        Peer::Channel(_) => true,
        Peer::Chat(_) => false,
    };
    if eligible {
        shown_status_id(peer, now)
    } else {
        None
    }
}

/// The emoji status of the profile header (`BadgeContentForPeer`): a user shows it only with
/// Premium and without the verified check (`BadgeValue` is `Verified` first), unless the status
/// is a collectible; a channel or supergroup whenever it has one.
#[must_use]
pub fn profile_emoji_status_id(peer: &Peer, now: i32) -> Option<u64> {
    if let Peer::User(u) = peer {
        let collectible = peer.common().emoji_status.collectible_id.is_some();
        let badge_is_premium = if u.is_verified() {
            collectible
        } else {
            u.is_premium()
        };
        if !badge_is_premium {
            return None;
        }
    }
    shown_status_id(peer, now)
}

/// Queues the emoji statuses of chat-list rows for the custom emoji loader (the row's
/// `customEmojiManager().create` in `PeerBadge::drawPremiumEmojiStatus`). Never blocks.
pub(crate) fn request_for_rows(
    live: &crate::session::LiveSession,
    rows: &[tg_app_core::dialogs::backend::DialogItem],
) {
    let ids: Vec<u64> = rows.iter().filter_map(|row| row.emoji_status_id).collect();
    if !ids.is_empty() {
        crate::custom_emoji_loader::request(live, &ids);
    }
}

/// Queues the profile header's emoji status for the custom emoji loader.
pub(crate) fn request_for_profile(
    live: &crate::session::LiveSession,
    profile: &tg_app_core::info::backend::PeerProfile,
) {
    if let Some(id) = profile.emoji_status_id {
        crate::custom_emoji_loader::request(live, &[id]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tg_data::{
        Channel, ChannelFlags, ChannelId, Chat, ChatId, DocumentId, User, UserFlags, UserId,
    };

    const STATUS: u64 = 5_550_001;
    const NOW: i32 = 1_000;

    fn user(premium: bool, verified: bool) -> User {
        let mut u = User::new(UserId(9), false);
        if premium {
            u.flags.insert(UserFlags::PREMIUM);
        }
        if verified {
            u.flags.insert(UserFlags::VERIFIED);
        }
        u.common.emoji_status.document_id = DocumentId(STATUS);
        u.common.bar_settings = PeerBarSettings::empty();
        u
    }

    fn channel(verified: bool) -> Peer {
        let mut ch = Channel::new(ChannelId(31));
        ch.flags.insert(ChannelFlags::BROADCAST);
        if verified {
            ch.flags.insert(ChannelFlags::VERIFIED);
        }
        ch.common.emoji_status.document_id = DocumentId(STATUS);
        Peer::Channel(Box::new(ch))
    }

    #[test]
    fn rows_show_the_status_of_premium_users_and_channels() {
        let premium = Peer::User(Box::new(user(true, false)));
        assert_eq!(row_emoji_status_id(&premium, NOW), Some(STATUS));
        let plain = Peer::User(Box::new(user(false, false)));
        assert_eq!(
            row_emoji_status_id(&plain, NOW),
            None,
            "no Premium, no status"
        );
        assert_eq!(row_emoji_status_id(&channel(false), NOW), Some(STATUS));
        let mut chat = Chat::new(ChatId(4));
        chat.common.emoji_status.document_id = DocumentId(STATUS);
        assert_eq!(row_emoji_status_id(&Peer::Chat(Box::new(chat)), NOW), None);
    }

    #[test]
    fn expired_and_spam_flagged_statuses_are_hidden() {
        let mut expired = user(true, false);
        expired.common.emoji_status.until = NOW;
        assert_eq!(
            row_emoji_status_id(&Peer::User(Box::new(expired)), NOW),
            None
        );
        let mut later = user(true, false);
        later.common.emoji_status.until = NOW + 1;
        assert_eq!(
            row_emoji_status_id(&Peer::User(Box::new(later)), NOW),
            Some(STATUS)
        );
        let mut spam = user(true, false);
        spam.common.bar_settings = PeerBarSettings::REPORT_SPAM;
        assert_eq!(row_emoji_status_id(&Peer::User(Box::new(spam)), NOW), None);
        let mut unknown = user(true, false);
        unknown.common.bar_settings = PeerBarSettings::UNKNOWN | PeerBarSettings::REPORT_SPAM;
        assert_eq!(
            row_emoji_status_id(&Peer::User(Box::new(unknown)), NOW),
            Some(STATUS),
            "settings not loaded yet hide nothing"
        );
    }

    #[test]
    fn the_profile_shows_a_verified_users_status_only_when_collectible() {
        let verified = Peer::User(Box::new(user(true, true)));
        assert_eq!(profile_emoji_status_id(&verified, NOW), None);
        let mut collectible = user(true, true);
        collectible.common.emoji_status.collectible_id = Some(tg_data::CollectibleId(3));
        assert_eq!(
            profile_emoji_status_id(&Peer::User(Box::new(collectible)), NOW),
            Some(STATUS)
        );
        let premium = Peer::User(Box::new(user(true, false)));
        assert_eq!(profile_emoji_status_id(&premium, NOW), Some(STATUS));
        assert_eq!(profile_emoji_status_id(&channel(true), NOW), Some(STATUS));
    }
}
