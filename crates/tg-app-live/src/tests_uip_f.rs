//! UI parity stage 2, package F: live data conversion matches Telegram Desktop 7.4.6.
//!
//! Each test builds a synthetic `tg_data::Session` (no network) and checks one converted value
//! against tdesktop's behavior: chat-list preview text and sender, the self peer's online state,
//! Saved Messages out layout, service texts, channel posting rights, real names for the owner and
//! the basic-group admin count.

use tg_app_core::dialogs::backend::DialogListQuery;
use tg_app_core::history::backend::ServiceAction as UiServiceAction;
use tg_app_core::info::backend::MemberRole;
use tg_app_core::stand_in::types::FilterId;
use tg_data::{
    Channel, ChannelFlags, ChannelId, Chat, ChatId, DocumentData, DocumentKind, ForwardInfo,
    LastseenStatus, Message, MessageContent, MessageFlags, MessageMedia, MsgId, PeerId,
    PeerUpdateFlags, PhotoData, ServiceAction, Session, TextWithEntities, User, UserId,
};

use crate::convert::{
    dialog_item_from_session, message_to_history_message, message_to_viewer_media_item,
    peer_to_info_profile, peer_to_peer_summary,
};
use crate::search_and_info::{build_member_entry, message_to_search_result};

const NOW: i32 = 1_764_000_000;
const SELF_ID: u64 = 1001;
const OTHER_ID: u64 = 1002;
const BOT_ID: u64 = 1003;

fn user(id: u64, is_self: bool, first: &str, last: &str) -> User {
    let mut u = User::new(UserId(id), is_self);
    u.set_name(first, last, "", "");
    u.lastseen = LastseenStatus::online_till_status(NOW + 300, false, false);
    u
}

fn session() -> Session {
    let mut s = Session::new(UserId(SELF_ID));
    s.upsert_user(
        user(SELF_ID, true, "Testname", "Testsurname"),
        PeerUpdateFlags::all(),
    );
    s.upsert_user(
        user(OTHER_ID, false, "Sample", "Person"),
        PeerUpdateFlags::all(),
    );
    let mut bot = user(BOT_ID, false, "Sample Attach Bot", "");
    bot.bot_info = Some(Default::default());
    s.upsert_user(bot, PeerUpdateFlags::all());
    s
}

fn self_peer() -> PeerId {
    PeerId::from_user(UserId(SELF_ID))
}

fn text_msg(peer: PeerId, id: i64, from: Option<PeerId>, text: &str) -> Message {
    Message::new_text(
        peer,
        MsgId(id),
        from,
        NOW - 60,
        TextWithEntities::plain(text),
    )
}

fn service_msg(peer: PeerId, id: i64, from: Option<PeerId>, action: ServiceAction) -> Message {
    let mut msg = text_msg(peer, id, from, "");
    msg.content = MessageContent::Service(Box::new(action));
    msg
}

fn media_msg(
    peer: PeerId,
    id: i64,
    from: Option<PeerId>,
    caption: &str,
    media: MessageMedia,
) -> Message {
    let mut msg = text_msg(peer, id, from, caption);
    msg.content = MessageContent::Regular {
        text: TextWithEntities::plain(caption),
        media,
    };
    msg
}

fn file_doc(name: &str) -> MessageMedia {
    MessageMedia::Document(Box::new(DocumentData {
        kind: DocumentKind::File,
        file_name: name.to_owned(),
        mime_type: "text/plain".to_owned(),
        size: 88,
        ..Default::default()
    }))
}

fn photo(id: u64) -> MessageMedia {
    MessageMedia::Photo(Box::new(PhotoData {
        id: tg_data::PhotoId(id),
        ..Default::default()
    }))
}

fn row_preview(s: &Session, peer: PeerId) -> (String, Option<String>, bool) {
    let item = dialog_item_from_session(s, peer, DialogListQuery::Filter(FilterId::ALL), NOW)
        .expect("dialog row");
    (item.last_message, item.last_sender, item.online)
}

#[test]
fn bot_allowed_preview_renders_tdesktop_text() {
    let mut s = session();
    let bot = PeerId::from_user(UserId(BOT_ID));
    s.insert_message(service_msg(
        bot,
        5,
        Some(bot),
        ServiceAction::BotAllowed {
            attach_menu: true,
            from_request: false,
            domain: None,
            app_id: None,
        },
    ));
    let (text, sender, _) = row_preview(&s, bot);
    assert_eq!(
        text,
        "You allowed this bot to message you when you added it to your attachment menu."
    );
    assert_eq!(sender, None);

    let item = s
        .message(tg_data::FullMsgId::new(bot, MsgId(5)))
        .expect("msg");
    let history = message_to_history_message(&s, item);
    assert_eq!(
        history.service,
        Some(UiServiceAction::Custom(
            "You allowed this bot to message you when you added it to your attachment menu."
                .to_owned()
        ))
    );
}

#[test]
fn file_and_photo_previews_use_tdesktop_text() {
    let mut s = session();
    s.insert_message(media_msg(
        self_peer(),
        7,
        Some(self_peer()),
        "",
        file_doc("sample_notes.txt"),
    ));
    let (text, sender, _) = row_preview(&s, self_peer());
    assert_eq!(text, "sample_notes.txt");
    assert_eq!(sender, None);

    let other = PeerId::from_user(UserId(OTHER_ID));
    s.insert_message(media_msg(other, 8, Some(other), "", photo(70)));
    assert_eq!(row_preview(&s, other).0, "Photo");
    s.insert_message(media_msg(
        other,
        9,
        Some(other),
        "Channel photo post",
        photo(71),
    ));
    assert_eq!(row_preview(&s, other).0, "Channel photo post");
    s.insert_message(media_msg(
        other,
        10,
        Some(other),
        "see this",
        file_doc("report.pdf"),
    ));
    assert_eq!(row_preview(&s, other).0, "report.pdf, see this");
}

#[test]
fn self_peer_is_never_online_in_chat_list() {
    let mut s = session();
    s.insert_message(text_msg(self_peer(), 3, Some(self_peer()), "note"));
    let (_, _, online) = row_preview(&s, self_peer());
    assert!(!online, "Saved Messages row must never show the online dot");

    let other = PeerId::from_user(UserId(OTHER_ID));
    s.insert_message(text_msg(other, 4, Some(other), "hi"));
    assert!(
        row_preview(&s, other).2,
        "a real online contact keeps the dot"
    );
}

#[test]
fn saved_messages_items_are_outgoing() {
    let mut s = session();
    let own = text_msg(self_peer(), 11, Some(self_peer()), "my note");
    assert!(!own.flags.contains(MessageFlags::OUTGOING));
    s.insert_message(own);
    let mut forwarded = text_msg(self_peer(), 12, Some(self_peer()), "from a contact");
    forwarded.forward_from = Some(ForwardInfo {
        from_id: Some(PeerId::from_user(UserId(OTHER_ID))),
        ..Default::default()
    });
    s.insert_message(forwarded);

    let own = s
        .message(tg_data::FullMsgId::new(self_peer(), MsgId(11)))
        .expect("own");
    assert!(message_to_history_message(&s, own).outgoing);
    let fwd = s
        .message(tg_data::FullMsgId::new(self_peer(), MsgId(12)))
        .expect("fwd");
    assert!(!message_to_history_message(&s, fwd).outgoing);
}

fn broadcast_channel(id: u64) -> Channel {
    let mut ch = Channel::new(ChannelId(id));
    ch.set_name("Parity Sample Channel", "");
    ch.flags.remove(ChannelFlags::FORBIDDEN);
    ch.flags
        .insert(ChannelFlags::BROADCAST | ChannelFlags::CREATOR);
    ch
}

#[test]
fn channel_creation_service_text_is_channel_created() {
    let mut s = session();
    s.upsert_channel(broadcast_channel(50), PeerUpdateFlags::all());
    let peer = PeerId::from_channel(ChannelId(50));
    let mut msg = service_msg(
        peer,
        1,
        None,
        ServiceAction::ChannelCreate {
            title: "Parity Sample Channel".to_owned(),
        },
    );
    msg.flags.insert(MessageFlags::POST);
    s.insert_message(msg);
    let item = s
        .message(tg_data::FullMsgId::new(peer, MsgId(1)))
        .expect("msg");
    assert_eq!(
        message_to_history_message(&s, item).service,
        Some(UiServiceAction::Custom("Channel created".to_owned()))
    );
    assert_eq!(row_preview(&s, peer).0, "Channel created");
}

#[test]
fn group_creation_uses_creator_real_name_and_guillemets() {
    let mut s = session();
    let mut chat = Chat::new(ChatId(60));
    chat.set_name("Parity Sample Group");
    s.upsert_chat(chat, PeerUpdateFlags::all());
    let peer = PeerId::from_chat(ChatId(60));
    s.insert_message(service_msg(
        peer,
        1,
        Some(self_peer()),
        ServiceAction::ChatCreate {
            title: "Parity Sample Group".to_owned(),
            users: vec![UserId(SELF_ID)],
        },
    ));
    let item = s
        .message(tg_data::FullMsgId::new(peer, MsgId(1)))
        .expect("msg");
    assert_eq!(
        message_to_history_message(&s, item).service,
        Some(UiServiceAction::Custom(
            "Testname Testsurname created the group \u{AB}Parity Sample Group\u{BB}".to_owned()
        ))
    );

    let mut own = text_msg(peer, 2, Some(self_peer()), "Welcome to the group");
    own.flags.insert(MessageFlags::OUTGOING);
    s.insert_message(own);
    let (text, sender, _) = row_preview(&s, peer);
    assert_eq!(text, "Welcome to the group");
    assert_eq!(sender.as_deref(), Some("You"));
}

#[test]
fn broadcast_creator_can_post_from_tl_without_direct_messages() {
    let mut s = session();
    let tl_ch = tg_tl::enums::Chat::Channel(tg_tl::types::Channel {
        creator: true,
        left: false,
        broadcast: true,
        verified: false,
        megagroup: false,
        restricted: false,
        signatures: false,
        min: false,
        scam: false,
        has_link: false,
        has_geo: false,
        slowmode_enabled: false,
        call_active: false,
        call_not_empty: false,
        fake: false,
        gigagroup: false,
        noforwards: false,
        join_to_send: false,
        join_request: false,
        forum: false,
        stories_hidden: false,
        stories_hidden_min: false,
        stories_unavailable: false,
        signature_profiles: false,
        autotranslation: false,
        broadcast_messages_allowed: false,
        monoforum: false,
        forum_tabs: false,
        in_news: false,
        forum_sidebar: false,
        id: 51,
        access_hash: Some(9001),
        title: "Parity Sample Channel".to_owned(),
        username: None,
        photo: tg_tl::enums::ChatPhoto::Empty,
        date: NOW,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(1),
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        emoji_status: None,
        level: None,
        subscription_until_date: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_monoforum_id: None,
        linked_community_id: None,
        pinned_msg_id: None,
    });
    tg_api::convert::apply_tl_chat(&mut s, &tl_ch).expect("apply broadcast channel");
    let ch = s.channel(ChannelId(51)).expect("channel");
    assert!(
        !ch.monoforum_disabled(),
        "tg-api must not set MONOFORUM_DISABLED on a broadcast channel"
    );
    let peer = s
        .peer(PeerId::from_channel(ChannelId(51)))
        .expect("channel");
    let summary = peer_to_peer_summary(peer, None, self_peer(), NOW, s.send_context());
    assert!(summary.can_send, "the owner gets the Broadcast composer");

    // A disabled monoforum channel, on the other hand, cannot be written to.
    let mut mf = broadcast_channel(52);
    mf.flags.remove(ChannelFlags::BROADCAST);
    mf.flags
        .insert(ChannelFlags::MONOFORUM | ChannelFlags::MONOFORUM_DISABLED);
    s.upsert_channel(mf, PeerUpdateFlags::all());
    let mf_peer = s.peer(PeerId::from_channel(ChannelId(52))).expect("mf");
    let mf_summary = peer_to_peer_summary(mf_peer, None, self_peer(), NOW, s.send_context());
    assert!(
        !mf_summary.can_send,
        "disabled monoforum must not be writable"
    );
}

#[test]
fn viewer_and_members_show_owner_real_name() {
    let mut s = session();
    s.insert_message(media_msg(
        self_peer(),
        20,
        None,
        "Sample landscape photo",
        photo(80),
    ));
    let item = s
        .message(tg_data::FullMsgId::new(self_peer(), MsgId(20)))
        .expect("msg");
    let viewer = message_to_viewer_media_item(&s, item).expect("viewer item");
    assert_eq!(viewer.sender_name, "Testname Testsurname");

    let member = build_member_entry(
        &s,
        self_peer(),
        MemberRole::Owner { custom_title: None },
        NOW,
    );
    assert_eq!(member.name, "Testname Testsurname");
    assert_eq!(member.status_text, "online");
    assert!(member.is_online);
}

#[test]
fn search_hits_follow_chat_list_sender_rules() {
    let mut s = session();
    s.insert_message(text_msg(
        self_peer(),
        30,
        Some(self_peer()),
        "Testing parity notes",
    ));
    let hit = s
        .message(tg_data::FullMsgId::new(self_peer(), MsgId(30)))
        .expect("msg");
    let result = message_to_search_result(&s, hit).expect("result");
    assert_eq!(result.sender_name, None, "no 'Saved Messages:' prefix");
    assert_eq!(result.text, "Testing parity notes");

    let other = PeerId::from_user(UserId(OTHER_ID));
    s.insert_message(text_msg(other, 31, Some(other), "1:1 parity text"));
    let hit = s
        .message(tg_data::FullMsgId::new(other, MsgId(31)))
        .expect("msg");
    assert_eq!(
        message_to_search_result(&s, hit)
            .expect("result")
            .sender_name,
        None
    );
}

#[test]
fn basic_group_admin_count_includes_creator() {
    let mut s = session();
    let mut chat = Chat::new(ChatId(61));
    chat.set_name("Parity Sample Group");
    chat.creator = UserId(SELF_ID);
    chat.participants.insert(UserId(SELF_ID));
    s.upsert_chat(chat, PeerUpdateFlags::all());
    let profile = peer_to_info_profile(&s, PeerId::from_chat(ChatId(61)), NOW).expect("profile");
    assert_eq!(profile.admins_count, 1);
}
