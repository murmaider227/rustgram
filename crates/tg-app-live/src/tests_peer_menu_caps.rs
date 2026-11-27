//! TGB-3183: the live session fills the rights the chat menu reads (`Window::Filler`), so a
//! supergroup member sees "Manage group", "Report", "Leave group" and the rest, not only the
//! mute row.

use tg_app_core::history::peer_menu::PeerMenuKind;
use tg_data::{
    Channel, ChannelFlags, ChannelId, Chat, ChatBannedRights, ChatFlags, ChatId, MsgId,
    PeerId as DataPeerId, PeerUpdateFlags, Session, User, UserId,
};

use crate::convert::from_data_peer;
use crate::peer_menu_caps::peer_menu_caps;

const SELF: UserId = UserId(100);

fn session() -> Session {
    let mut s = Session::new(SELF);
    s.upsert_user(User::new(SELF, true), PeerUpdateFlags::empty());
    s
}

fn add_channel(s: &mut Session, id: u64, flags: ChannelFlags, banned: ChatBannedRights) {
    let mut channel = Channel::new(ChannelId(id));
    channel.set_name("Channel", "");
    channel.flags = flags;
    channel.default_restrictions = banned;
    s.upsert_channel(channel, PeerUpdateFlags::empty());
    s.ensure_history(DataPeerId::from_channel(ChannelId(id)))
        .top_message_id = Some(MsgId(10));
}

#[test]
fn supergroup_member_gets_manage_report_and_leave_rights() {
    let mut s = session();
    add_channel(
        &mut s,
        501,
        ChannelFlags::MEGAGROUP,
        ChatBannedRights::empty(),
    );
    let caps =
        peer_menu_caps(&s, from_data_peer(DataPeerId::from_channel(ChannelId(501)))).expect("caps");
    assert_eq!(caps.kind, PeerMenuKind::Megagroup);
    assert!(caps.am_in);
    assert!(!caps.am_creator, "a member may report the group");
    // `ChannelData::canEditInformation`: a supergroup member may change the info unless the
    // group restricts it, so `EditPeerInfoBox::Available` is true.
    assert!(caps.can_manage);
    assert!(caps.can_create_polls);
    assert!(caps.can_export);
    assert!(!caps.can_delete_messages);
}

#[test]
fn supergroup_with_change_info_restricted_has_no_manage_row() {
    let mut s = session();
    add_channel(
        &mut s,
        502,
        ChannelFlags::MEGAGROUP,
        ChatBannedRights::CHANGE_INFO | ChatBannedRights::SEND_POLLS,
    );
    let caps =
        peer_menu_caps(&s, from_data_peer(DataPeerId::from_channel(ChannelId(502)))).expect("caps");
    assert!(!caps.can_manage);
    assert!(!caps.can_create_polls);
}

#[test]
fn broadcast_subscriber_and_left_channel() {
    let mut s = session();
    add_channel(
        &mut s,
        503,
        ChannelFlags::BROADCAST,
        ChatBannedRights::empty(),
    );
    add_channel(
        &mut s,
        504,
        ChannelFlags::BROADCAST | ChannelFlags::LEFT,
        ChatBannedRights::empty(),
    );
    let sub =
        peer_menu_caps(&s, from_data_peer(DataPeerId::from_channel(ChannelId(503)))).expect("caps");
    assert_eq!(sub.kind, PeerMenuKind::Broadcast);
    assert!(sub.am_in && !sub.can_manage && !sub.can_create_polls);
    let left =
        peer_menu_caps(&s, from_data_peer(DataPeerId::from_channel(ChannelId(504)))).expect("caps");
    assert!(!left.am_in);
}

#[test]
fn basic_group_creator_and_private_chats() {
    let mut s = session();
    let mut chat = Chat::new(ChatId(701));
    chat.set_name("Group");
    chat.flags = ChatFlags::CREATOR;
    s.upsert_chat(chat, PeerUpdateFlags::empty());
    let caps =
        peer_menu_caps(&s, from_data_peer(DataPeerId::from_chat(ChatId(701)))).expect("caps");
    assert_eq!(caps.kind, PeerMenuKind::BasicGroup);
    assert!(caps.am_creator && caps.can_manage && caps.can_delete_messages);
    assert!(!caps.can_export, "no messages loaded yet");

    let other = UserId(200);
    s.upsert_user(User::new(other, false), PeerUpdateFlags::empty());
    let user = peer_menu_caps(&s, from_data_peer(DataPeerId::from_user(other))).expect("caps");
    assert_eq!(user.kind, PeerMenuKind::User);
    assert!(!user.can_manage && !user.can_create_polls);
    let me = peer_menu_caps(&s, from_data_peer(DataPeerId::from_user(SELF))).expect("caps");
    assert_eq!(me.kind, PeerMenuKind::SavedMessages);
    assert!(me.can_create_polls);
}

#[test]
fn direct_messages_need_a_monoforum_and_access_to_it() {
    let mut s = session();
    add_channel(
        &mut s,
        505,
        ChannelFlags::BROADCAST | ChannelFlags::CREATOR,
        ChatBannedRights::empty(),
    );
    add_channel(
        &mut s,
        506,
        ChannelFlags::BROADCAST,
        ChatBannedRights::empty(),
    );
    let peer = |id| from_data_peer(DataPeerId::from_channel(ChannelId(id)));
    assert!(!peer_menu_caps(&s, peer(505)).expect("caps").direct_messages);
    for id in [505, 506] {
        if let Some(ch) = s.channel_mut(ChannelId(id)) {
            ch.monoforum_link = Some(ChannelId(id + 100));
        }
    }
    // `amMonoforumAdmin` follows the broadcast's `canAccessMonoforum()`.
    assert!(peer_menu_caps(&s, peer(505)).expect("caps").direct_messages);
    assert!(!peer_menu_caps(&s, peer(506)).expect("caps").direct_messages);
}
