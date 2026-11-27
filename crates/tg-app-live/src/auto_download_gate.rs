//! Auto-download gate for chat photos (`Data::AutoDownload::Should(settings, peer, photo)`,
//! `data/data_auto_download.cpp:131-136,406-424`) and autoplay of round video messages
//! (`Data::AutoDownload::ShouldAutoPlay`).

use tg_app_core::settings::backend::{AutoDownloadSettings, AutoDownloadSource, AutoDownloadType};
use tg_data::{DocumentKind, Message, MessageContent, MessageMedia, PeerId as DataPeerId, Session};

use crate::media::photo_to_location;

/// Auto-download source of `peer` (`SourceFromPeer`): private chats, groups (basic groups and
/// megagroups) or channels.
#[must_use]
pub fn source_for_peer(session: &Session, peer: DataPeerId) -> AutoDownloadSource {
    match session.peer(peer) {
        Some(p) if p.as_chat().is_some() => AutoDownloadSource::Groups,
        Some(p) => match p.as_channel() {
            Some(c) if c.is_megagroup() => AutoDownloadSource::Groups,
            Some(_) => AutoDownloadSource::Channels,
            None => AutoDownloadSource::Private,
        },
        None if peer.is_chat() => AutoDownloadSource::Groups,
        None if peer.is_channel() => AutoDownloadSource::Channels,
        None => AutoDownloadSource::Private,
    }
}

/// Whether `msg`'s photo (if any) may be downloaded automatically: the limit for its chat type
/// is on and not below the size of the photo the bubble shows (`Single::shouldDownload`).
/// Messages without a photo always return `true`.
#[must_use]
pub fn photo_autoload_allowed(
    session: &Session,
    settings: &AutoDownloadSettings,
    msg: &Message,
) -> bool {
    let MessageContent::Regular { media, .. } = &msg.content else {
        return true;
    };
    let photo = match media {
        MessageMedia::Photo(photo) => photo,
        MessageMedia::WebPage(wp) => match wp.photo.as_ref() {
            Some(photo) => photo,
            None => return true,
        },
        _ => return true,
    };
    let size = i64::try_from(photo_to_location(photo).size).unwrap_or(i64::MAX);
    let limit = settings.bytes_limit(
        source_for_peer(session, msg.peer_id),
        AutoDownloadType::Photo,
    );
    limit > 0 && size <= limit
}

/// Whether `msg`'s round video message (if any) may play, and so download, by itself when it
/// enters the list (`ShouldAutoPlay` with `AutoPlayVideoMessage`, TGB-2997). tdesktop never
/// downloads a video message automatically otherwise (`Should` is false for it). Messages
/// without a round video always return `true`.
#[must_use]
pub fn round_video_autoplay_allowed(
    session: &Session,
    settings: &AutoDownloadSettings,
    msg: &Message,
) -> bool {
    let MessageContent::Regular {
        media: MessageMedia::Document(doc),
        ..
    } = &msg.content
    else {
        return true;
    };
    if doc.kind != DocumentKind::RoundVideo {
        return true;
    }
    let limit = settings.bytes_limit(
        source_for_peer(session, msg.peer_id),
        AutoDownloadType::AutoPlayVideoMessage,
    );
    settings.autoplay_round_videos && limit > 0 && doc.size <= limit
}

#[cfg(test)]
mod tests {
    use super::*;
    use tg_data::TextWithEntities;
    use tg_data::{ChannelId, MsgId, PhotoData, PhotoId, PhotoSize, UserId};

    fn photo_msg(peer: DataPeerId, size: i64) -> Message {
        let mut msg = Message::new_text(
            peer,
            MsgId(1),
            None,
            1_760_000_000,
            TextWithEntities::plain(String::new()),
        );
        let photo = PhotoData {
            id: PhotoId(9),
            sizes: vec![PhotoSize {
                kind: "y".to_owned(),
                width: 1280,
                height: 960,
                size,
            }],
            ..PhotoData::default()
        };
        msg.content = MessageContent::Regular {
            text: TextWithEntities::plain(String::new()),
            media: MessageMedia::Photo(Box::new(photo)),
        };
        msg
    }

    #[test]
    fn photos_follow_the_per_source_limit() {
        let session = Session::new(UserId(1));
        let user = DataPeerId::from_user(UserId(5));
        let channel = DataPeerId::from_channel(ChannelId(6));
        let mut settings = AutoDownloadSettings::default();
        assert!(photo_autoload_allowed(
            &session,
            &settings,
            &photo_msg(user, 200_000)
        ));
        assert!(photo_autoload_allowed(
            &session,
            &settings,
            &photo_msg(channel, 200_000)
        ));

        settings.limits[AutoDownloadSource::Channels as usize][AutoDownloadType::Photo as usize] =
            0;
        assert!(
            !photo_autoload_allowed(&session, &settings, &photo_msg(channel, 200_000)),
            "photos off for channels"
        );
        assert!(photo_autoload_allowed(
            &session,
            &settings,
            &photo_msg(user, 200_000)
        ));

        settings.limits[AutoDownloadSource::Private as usize][AutoDownloadType::Photo as usize] =
            100_000;
        assert!(
            !photo_autoload_allowed(&session, &settings, &photo_msg(user, 200_000)),
            "larger than the limit"
        );
    }
}
