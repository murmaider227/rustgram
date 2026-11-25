//! UI parity stage 4, package F: delivery ticks of chat-list rows and search results
//! (`HistoryItem::unread`).

use tg_app_core::dialogs::backend::DialogListQuery;
use tg_app_core::stand_in::types::FilterId;
use tg_data::{
    Channel, ChannelFlags, ChannelId, Chat, ChatId, Message, MessageFlags, MsgId, PeerId,
    PeerUpdateFlags, Session, TextWithEntities, User, UserId,
};
use tg_history::DialogSendState;

use crate::convert::{dialog_item_from_session, message_send_state};
use crate::search_and_info::message_to_search_result;

const NOW: i32 = 1_795_568_820;
const ME: u64 = 1001;

fn session_with_peers() -> Session {
    let mut s = Session::new(UserId(ME));
    s.upsert_user(User::new(UserId(ME), true), PeerUpdateFlags::all());
    let mut friend = User::new(UserId(2001), false);
    friend.set_name("Sample", "Friend", "", "");
    s.upsert_user(friend, PeerUpdateFlags::all());
    let mut bot = User::new(UserId(2002), false);
    bot.set_name("Sample Bot", "", "", "");
    bot.bot_info = Some(Default::default());
    s.upsert_user(bot, PeerUpdateFlags::all());

    let mut chat = Chat::new(ChatId(70));
    chat.set_name("Sample Group");
    s.upsert_chat(chat, PeerUpdateFlags::all());

    let mut broadcast = Channel::new(ChannelId(80));
    broadcast.set_name("Sample Channel", "");
    broadcast.set_flags(ChannelFlags::BROADCAST | ChannelFlags::CREATOR);
    s.upsert_channel(broadcast, PeerUpdateFlags::all());

    let mut mega = Channel::new(ChannelId(81));
    mega.set_name("Sample Supergroup", "");
    mega.set_flags(ChannelFlags::MEGAGROUP);
    s.upsert_channel(mega, PeerUpdateFlags::all());
    s
}

fn send_out(s: &mut Session, peer: PeerId, id: i64) {
    let me = PeerId::from_user(UserId(ME));
    let mut m = Message::new_text(
        peer,
        MsgId(id),
        Some(me),
        NOW,
        TextWithEntities::plain("Parity check"),
    );
    m.flags.insert(MessageFlags::OUTGOING);
    s.insert_message(m);
}

fn row_state(s: &Session, peer: PeerId) -> DialogSendState {
    dialog_item_from_session(s, peer, DialogListQuery::Filter(FilterId::ALL), NOW)
        .expect("dialog row")
        .send_state
}

#[test]
fn unread_outgoing_ticks_follow_history_item_unread() {
    let mut s = session_with_peers();
    let friend = PeerId::from_user(UserId(2001));
    let bot = PeerId::from_user(UserId(2002));
    let group = PeerId::from_chat(ChatId(70));
    let broadcast = PeerId::from_channel(ChannelId(80));
    let mega = PeerId::from_channel(ChannelId(81));
    for (i, peer) in [friend, bot, group, broadcast, mega]
        .into_iter()
        .enumerate()
    {
        send_out(&mut s, peer, 10 + i as i64);
    }

    // Nobody has read anything yet: one tick where the peer can read messages ...
    assert_eq!(row_state(&s, friend), DialogSendState::Sent);
    assert_eq!(row_state(&s, group), DialogSendState::Sent);
    assert_eq!(row_state(&s, mega), DialogSendState::Sent);
    // ... and two ticks for bots and broadcast channels, which never report reads.
    assert_eq!(row_state(&s, bot), DialogSendState::Received);
    assert_eq!(row_state(&s, broadcast), DialogSendState::Received);

    // Once the peer reads it, two ticks.
    if let Some(h) = s.history_mut(friend) {
        h.outbox_read(MsgId(10));
    }
    assert_eq!(row_state(&s, friend), DialogSendState::Received);
}

#[test]
fn search_results_carry_the_same_ticks_as_rows() {
    let mut s = session_with_peers();
    let me = PeerId::from_user(UserId(ME));
    let friend = PeerId::from_user(UserId(2001));
    let broadcast = PeerId::from_channel(ChannelId(80));
    send_out(&mut s, friend, 20);
    send_out(&mut s, broadcast, 21);
    send_out(&mut s, me, 22);
    let mut incoming = Message::new_text(
        friend,
        MsgId(23),
        Some(friend),
        NOW,
        TextWithEntities::plain("Parity reply"),
    );
    incoming.flags.remove(MessageFlags::OUTGOING);
    s.insert_message(incoming);

    let state = |peer: PeerId, id: i64| {
        let m = s
            .message(tg_data::FullMsgId::new(peer, MsgId(id)))
            .expect("message");
        let result = message_to_search_result(&s, m).expect("search result");
        assert_eq!(result.send_state, message_send_state(&s, m));
        result.send_state
    };
    assert_eq!(state(friend, 20), DialogSendState::Sent);
    assert_eq!(state(broadcast, 21), DialogSendState::Received);
    // Saved Messages and incoming messages draw no ticks.
    assert_eq!(state(me, 22), DialogSendState::None);
    assert_eq!(state(friend, 23), DialogSendState::None);
}
