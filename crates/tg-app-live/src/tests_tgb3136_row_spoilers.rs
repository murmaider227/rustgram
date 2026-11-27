//! TGB-3136: the chat-list row and the search result of a message with a spoiler carry the
//! spoiler's byte ranges, so the painters hide them (`dialogsTextPalette.spoilerFg`) instead of
//! showing the text in clear.

use tg_app_core::dialogs::backend::DialogListQuery;
use tg_app_core::stand_in::types::FilterId;
use tg_data::{
    EntityKind, Message, MsgId, PeerId, PeerUpdateFlags, Session, TextEntity, TextWithEntities,
    User, UserId,
};

use crate::convert::dialog_item_from_session;
use crate::search_and_info::message_to_search_result;

const NOW: i32 = 1_795_568_820;
const ME: u64 = 1001;
const FRIEND: u64 = 2001;

fn utf16_len(s: &str) -> i32 {
    i32::try_from(s.encode_utf16().count()).unwrap_or(i32::MAX)
}

fn session_with_spoiler_message(text: &str, hidden: &str) -> (Session, PeerId) {
    let mut s = Session::new(UserId(ME));
    s.upsert_user(User::new(UserId(ME), true), PeerUpdateFlags::all());
    let mut friend = User::new(UserId(FRIEND), false);
    friend.set_name("Sample", "Friend", "", "");
    s.upsert_user(friend, PeerUpdateFlags::all());
    let peer = PeerId::from_user(UserId(FRIEND));
    let at = text.find(hidden).expect("hidden part is in the text");
    let body = TextWithEntities {
        text: text.to_owned(),
        entities: vec![TextEntity::new(
            utf16_len(&text[..at]),
            utf16_len(hidden),
            EntityKind::Spoiler,
        )],
    };
    s.insert_message(Message::new_text(peer, MsgId(10), Some(peer), NOW, body));
    (s, peer)
}

fn shown<'a>(text: &'a str, ranges: &[std::ops::Range<usize>]) -> Vec<&'a str> {
    ranges.iter().map(|r| &text[r.clone()]).collect()
}

#[test]
fn the_chat_list_row_keeps_the_spoiler_of_its_last_message() {
    let (s, peer) = session_with_spoiler_message("the code is 4711, keep it", "4711");
    let row = dialog_item_from_session(&s, peer, DialogListQuery::Filter(FilterId::ALL), NOW)
        .expect("dialog row");
    assert_eq!(row.last_message, "the code is 4711, keep it");
    assert_eq!(
        shown(&row.last_message, &row.last_message_spoilers),
        ["4711"]
    );
}

#[test]
fn a_search_result_keeps_the_spoiler_on_one_line() {
    let (s, peer) = session_with_spoiler_message("first line\nsecret ünïcode\nlast", "ünïcode");
    let msg = s
        .message(tg_data::FullMsgId::new(peer, MsgId(10)))
        .expect("stored message");
    let result = message_to_search_result(&s, msg).expect("search result");
    assert_eq!(result.text, "first line secret ünïcode last");
    assert_eq!(shown(&result.text, &result.text_spoilers), ["ünïcode"]);
}
