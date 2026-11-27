//! TGB-3012: sender and reply names take the peer's server colour, like tdesktop's
//! `Element::colorIndex` / `Reply::_colorPeer`, not `id % 7`.

use tg_app_core::themes::backend::PeerColorOptionRecord;
use tg_data::changes::PeerUpdateFlags;
use tg_data::{
    Channel, ChannelFlags, ChannelId, ColorCollectibleAccent, ForwardInfo, Message, MsgId, PeerId,
    ReplyInfo, Session, TextWithEntities, User, UserId,
};
use tg_theme::Color;
use tg_ui::peer_color::{CollectibleAccent, ColorIndexData, PeerAccent};

use super::{color_indices_from, content_colors_from, peer_accent};
use crate::convert::{ReplyTargetState, message_to_history_message, reply_header_for};

/// `777000 % 7 == 0`: red by id.
const RED_BY_ID: u64 = 777_000;
const GROUP: ChannelId = ChannelId(50);

fn session_with_user(id: u64, color: Option<u8>) -> Session {
    let mut session = Session::new(UserId(99));
    add_user(&mut session, id, color);
    let mut ch = Channel::new(GROUP);
    ch.flags.insert(ChannelFlags::MEGAGROUP);
    ch.flags.remove(ChannelFlags::FORBIDDEN);
    session.upsert_channel(ch, PeerUpdateFlags::empty());
    session
}

fn add_user(session: &mut Session, id: u64, color: Option<u8>) {
    let mut user = User::new(UserId(id), false);
    user.common.name = format!("User {id}");
    if let Some(index) = color {
        user.common.set_color_index(index);
    }
    session.upsert_user(user, PeerUpdateFlags::empty());
}

fn group_message(id: i64, from: u64) -> Message {
    Message::new_text(
        PeerId::from_channel(GROUP),
        MsgId(id),
        Some(PeerId::from_user(UserId(from))),
        1_700_000_000,
        TextWithEntities::default(),
    )
}

#[test]
fn sender_accent_is_the_server_colour_index_not_the_id() {
    assert_eq!(RED_BY_ID % 7, 0);
    let session = session_with_user(RED_BY_ID, Some(5));
    let hm = message_to_history_message(&session, &group_message(10, RED_BY_ID));
    assert_eq!(
        hm.extra.sender_accent,
        Some(PeerAccent {
            index: 5,
            collectible: None
        })
    );

    // Without a server colour the id decides (`Data::DecideColorIndex`).
    let session = session_with_user(RED_BY_ID, None);
    let hm = message_to_history_message(&session, &group_message(10, RED_BY_ID));
    assert_eq!(hm.extra.sender_accent.map(|a| a.index), Some(0));

    // Indexes past the seven palette colours stay as they are for the colour sets.
    let session = session_with_user(RED_BY_ID, Some(12));
    let hm = message_to_history_message(&session, &group_message(10, RED_BY_ID));
    assert_eq!(hm.extra.sender_accent.map(|a| a.index), Some(12));
}

#[test]
fn collectible_colour_gives_its_day_and_night_accent() {
    let mut session = session_with_user(RED_BY_ID, None);
    let pid = PeerId::from_user(UserId(RED_BY_ID));
    if let Some(peer) = session.peer_mut(pid) {
        peer.common_mut().color_collectible = Some(ColorCollectibleAccent {
            accent: 0x00ff_8800,
            dark_accent: Some(0x0011_2233),
        });
    }
    assert_eq!(
        peer_accent(&session, pid).collectible,
        Some(CollectibleAccent {
            day: 0x00ff_8800,
            night: 0x0011_2233,
        })
    );
}

#[test]
fn reply_author_accent_follows_the_replied_message_and_its_forward_origin() {
    const AUTHOR: u64 = 1_000_001;
    const ORIGIN: u64 = 1_000_002;
    let mut session = session_with_user(AUTHOR, Some(3));
    add_user(&mut session, ORIGIN, Some(6));
    let target = group_message(5, AUTHOR);
    let mut reply = group_message(6, RED_BY_ID);
    reply.reply_to = Some(ReplyInfo {
        reply_to_msg_id: Some(MsgId(5)),
        ..ReplyInfo::default()
    });

    let header = reply_header_for(&session, &reply, ReplyTargetState::Found(&target));
    assert_eq!(
        header.and_then(|h| h.author_accent).map(|a| a.index),
        Some(3)
    );

    // `HistoryItem::contentColorsFrom`: a forwarded target takes its original sender's colour.
    let mut forwarded = target.clone();
    forwarded.forward_from = Some(ForwardInfo {
        from_id: Some(PeerId::from_user(UserId(ORIGIN))),
        ..ForwardInfo::default()
    });
    assert_eq!(
        content_colors_from(&forwarded),
        PeerId::from_user(UserId(ORIGIN))
    );
    let header = reply_header_for(&session, &reply, ReplyTargetState::Found(&forwarded));
    assert_eq!(
        header.and_then(|h| h.author_accent).map(|a| a.index),
        Some(6)
    );
}

#[test]
fn colour_sets_fill_their_index_and_reuse_light_for_missing_dark() {
    let option = |color_id: u8, light: Vec<Color>, dark: Vec<Color>| PeerColorOptionRecord {
        color_id,
        hidden: false,
        channel_min_level: None,
        group_min_level: None,
        light_colors: light,
        dark_colors: dark,
    };
    let table = color_indices_from(&[
        option(
            10,
            vec![Color::rgb(0x3e, 0x88, 0xf7), Color::rgb(0xff, 0x32, 0xa0)],
            vec![Color::rgb(0x52, 0xbf, 0xff), Color::rgb(0xff, 0x59, 0x7b)],
        ),
        option(11, vec![Color::rgb(0x12, 0x34, 0x56)], Vec::new()),
        option(12, Vec::new(), Vec::new()),
    ]);
    assert_eq!(
        table[10],
        ColorIndexData {
            light: [Some(0x3e_88_f7), Some(0xff_32_a0), None],
            dark: [Some(0x52_bf_ff), Some(0xff_59_7b), None],
        }
    );
    assert_eq!(table[11].dark, [Some(0x12_34_56), None, None]);
    assert_eq!(table[12], ColorIndexData::default());
    assert_eq!(table[0], ColorIndexData::default());
}
