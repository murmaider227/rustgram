//! A history with two loaded ranges and unloaded messages between them (TGB-3176).
//!
//! A fake server holds a chat of [`TOTAL`] messages and answers `messages.getHistory` page by
//! page by its `offset_id` / `add_offset` / `limit`, like the real one. The client starts from
//! the dialog's last message only. Each test first makes a second loaded range far from the
//! bottom (opening at the first unread message, a jump to the first message, a page that reached
//! the top earlier), then scrolls through the history the way the history view does
//! (`try_load_slice_before` from its first message, `try_load_slice_after` from its last one)
//! and checks that every message comes exactly once, in order: the messages between the two
//! ranges must be requested, not skipped (tdesktop keeps one contiguous block per history and
//! loads from `minMsgId()` / `maxMsgId()`, `HistoryWidget::loadMessages` / `loadMessagesDown`).

use std::time::{Duration, Instant};

use tg_data::{
    Chat, ChatId, MsgId as DataMsgId, MsgRange, PeerId as DataPeerId, PeerUpdateFlags,
    UserId as DataUserId,
};
use tg_tl::types as tl_types;
use tg_tl::{Deserializable, Identifiable, Serializable, enums as tl_enums, functions as tl_fns};

use super::tests_reply_targets::{FakeRpcClient, PEER_ID, SELF_ID, build_live, tl_message};
use crate::LiveSession;
use tg_app_core::history::backend::{HistoryBackend, HistorySlice, Loadable};
use tg_app_core::stand_in::types::{MsgId, PeerId};

/// Messages in the chat, ids `1..=TOTAL`.
const TOTAL: i32 = 330;
/// The history view's page sizes (`OPEN_PAGE_BEFORE` / `OPEN_PAGE_AFTER`, scroll pages of 40).
const OPEN_BEFORE: usize = 60;
const OPEN_AFTER: usize = 40;
const SCROLL_PAGE: usize = 40;
/// The megagroup `build_live` creates.
const GROUP_ID: u64 = 700;
const BASIC_GROUP_ID: u64 = 71;

#[derive(Clone, Copy, Debug)]
enum Kind {
    Supergroup,
    BasicGroup,
    Private,
}

impl Kind {
    fn data_peer(self) -> DataPeerId {
        match self {
            Self::Supergroup => DataPeerId::from_channel(tg_data::ChannelId(GROUP_ID)),
            Self::BasicGroup => DataPeerId::from_chat(ChatId(BASIC_GROUP_ID)),
            Self::Private => DataPeerId::from_user(DataUserId(PEER_ID as u64)),
        }
    }

    fn tl_peer(self) -> tl_enums::Peer {
        match self {
            Self::Supergroup => tl_enums::Peer::Channel(tl_types::PeerChannel {
                channel_id: GROUP_ID as i64,
            }),
            Self::BasicGroup => tl_enums::Peer::Chat(tl_types::PeerChat {
                chat_id: BASIC_GROUP_ID as i64,
            }),
            Self::Private => tl_enums::Peer::User(tl_types::PeerUser { user_id: PEER_ID }),
        }
    }

    fn message(self, id: i32) -> tl_enums::Message {
        let from = if id % 3 == 0 { SELF_ID } else { PEER_ID };
        tl_message(id, self.tl_peer(), from, &format!("message {id}"), None)
    }
}

/// `messages.getHistory` the way the server answers it: newest first, starting at the first
/// message older than `offset_id` (the newest one for `0`) moved by `add_offset`.
fn serve_page(kind: Kind, req: &tl_fns::messages::GetHistory) -> Vec<u8> {
    let newest_first: Vec<i32> = (1..=TOTAL).rev().collect();
    let pos = if req.offset_id == 0 {
        0
    } else {
        newest_first
            .iter()
            .position(|&id| id < req.offset_id)
            .unwrap_or(newest_first.len())
    } as i64;
    let len = newest_first.len() as i64;
    let start = (pos + i64::from(req.add_offset)).clamp(0, len);
    let end = (pos + i64::from(req.add_offset) + i64::from(req.limit)).clamp(start, len);
    let messages: Vec<tl_enums::Message> = newest_first[start as usize..end as usize]
        .iter()
        .map(|&id| kind.message(id))
        .collect();
    match kind {
        Kind::Supergroup => {
            tl_enums::messages::Messages::ChannelMessages(tl_types::messages::ChannelMessages {
                inexact: false,
                pts: 1,
                count: TOTAL,
                offset_id_offset: None,
                messages,
                topics: Vec::new(),
                chats: Vec::new(),
                users: Vec::new(),
            })
            .to_bytes()
        }
        Kind::BasicGroup | Kind::Private => {
            tl_enums::messages::Messages::Slice(tl_types::messages::MessagesSlice {
                inexact: false,
                count: TOTAL,
                next_rate: None,
                offset_id_offset: None,
                search_flood: None,
                messages,
                topics: Vec::new(),
                chats: Vec::new(),
                users: Vec::new(),
            })
            .to_bytes()
        }
    }
}

/// A session that knows only the dialog's last message, with the fake server behind it.
fn setup(kind: Kind) -> (FakeRpcClient, LiveSession, PeerId) {
    let (rpc, live) = build_live(kind.data_peer(), vec![kind.message(TOTAL)]);
    if matches!(kind, Kind::BasicGroup) {
        live.write_session(|s| {
            let mut chat = Chat::new(ChatId(BASIC_GROUP_ID));
            chat.set_name("Basic Group");
            s.upsert_chat(chat, PeerUpdateFlags::all());
        });
    }
    rpc.set_handler(move |ctor, bytes| {
        if ctor != tl_fns::messages::GetHistory::CONSTRUCTOR_ID {
            return None;
        }
        let req = tl_fns::messages::GetHistory::from_bytes(bytes).ok()?;
        Some(serve_page(kind, &req))
    });
    (rpc, live, PeerId(kind.data_peer().0))
}

/// Asks like the history view does every frame until the page is there.
fn ready(mut ask: impl FnMut() -> Loadable<HistorySlice>) -> HistorySlice {
    let start = Instant::now();
    loop {
        match ask() {
            Loadable::Ready(slice) => return slice,
            Loadable::Failed => panic!("the page failed to load"),
            Loadable::Loading => {}
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the page never arrived"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The ids the history view shows and whether it thinks it reached each end.
struct View {
    ids: Vec<i32>,
    at_top: bool,
    at_bottom: bool,
}

impl View {
    fn open(live: &LiveSession, peer: PeerId, around: MsgId) -> Self {
        let slice = ready(|| live.try_load_slice_around(peer, around, OPEN_BEFORE, OPEN_AFTER));
        Self {
            ids: slice.messages.iter().map(|m| m.id.0).collect(),
            at_top: slice.loaded_at_top,
            at_bottom: slice.loaded_at_bottom,
        }
    }

    /// `HistoryInner::load_older_slice` until the view reaches the top.
    fn scroll_to_top(&mut self, live: &LiveSession, peer: PeerId) {
        for _ in 0..100 {
            if self.at_top {
                return;
            }
            let Some(&first) = self.ids.first() else {
                return;
            };
            let older = ready(|| live.try_load_slice_before(peer, MsgId(first), SCROLL_PAGE));
            if older.messages.is_empty() {
                self.at_top = true;
                return;
            }
            let mut ids: Vec<i32> = older.messages.iter().map(|m| m.id.0).collect();
            ids.append(&mut self.ids);
            self.ids = ids;
            self.at_top = older.loaded_at_top;
        }
        panic!("scrolling up never reached the top");
    }

    /// `HistoryInner::load_newer_slice` until the view reaches the bottom.
    fn scroll_to_bottom(&mut self, live: &LiveSession, peer: PeerId) {
        for _ in 0..100 {
            if self.at_bottom {
                return;
            }
            let Some(&last) = self.ids.last() else {
                return;
            };
            let newer = ready(|| live.try_load_slice_after(peer, MsgId(last), SCROLL_PAGE));
            if newer.messages.is_empty() {
                self.at_bottom = true;
                return;
            }
            self.ids.extend(newer.messages.iter().map(|m| m.id.0));
            self.at_bottom = newer.loaded_at_bottom;
        }
        panic!("scrolling down never reached the bottom");
    }
}

fn assert_whole_history(kind: Kind, what: &str, ids: &[i32]) {
    let expected: Vec<i32> = (1..=TOTAL).collect();
    if ids != expected.as_slice() {
        let missing: Vec<i32> = expected
            .iter()
            .copied()
            .filter(|id| !ids.contains(id))
            .collect();
        panic!(
            "{kind:?}, {what}: the view shows {} of {TOTAL} messages, first {:?}, last {:?}; \
             missing {} ({:?}...), in order and unique: {}",
            ids.len(),
            ids.first(),
            ids.last(),
            missing.len(),
            &missing[..missing.len().min(10)],
            ids.windows(2).all(|w| w[0] < w[1]),
        );
    }
}

/// The chat has unread messages from the third one on: the first open loads the page around
/// it (`History::loadAroundId`), the user scrolls up to the very first message, then returns to
/// the newest one and scrolls the whole history up again.
fn open_at_unread_then_scroll_up_from_the_end(kind: Kind) {
    let (_rpc, live, peer) = setup(kind);
    live.write_session(|s| {
        let h = s.ensure_history(kind.data_peer());
        h.unread_count = Some(TOTAL - 2);
        h.inbox_read_before = Some(DataMsgId(3));
    });
    let mut at_unread = View::open(&live, peer, MsgId::SHOW_AT_UNREAD);
    assert!(
        at_unread.ids.contains(&3),
        "{kind:?}: the first unread message opens: {:?}",
        at_unread.ids
    );
    at_unread.scroll_to_top(&live, peer);
    assert_eq!(at_unread.ids.first(), Some(&1), "{kind:?}");

    let mut at_end = View::open(&live, peer, MsgId::SHOW_AT_END);
    assert_eq!(at_end.ids.last(), Some(&TOTAL), "{kind:?}: the end opens");
    at_end.scroll_to_top(&live, peer);
    assert_whole_history(kind, "scrolled up from the end", &at_end.ids);
}

/// The mirror case: from the first unread message near the top, scrolling down must reach the
/// newest message through every message in between.
fn open_at_unread_then_scroll_down(kind: Kind) {
    let (_rpc, live, peer) = setup(kind);
    live.write_session(|s| {
        let h = s.ensure_history(kind.data_peer());
        h.unread_count = Some(TOTAL - 2);
        h.inbox_read_before = Some(DataMsgId(3));
    });
    let mut view = View::open(&live, peer, MsgId::SHOW_AT_UNREAD);
    view.scroll_to_top(&live, peer);
    view.scroll_to_bottom(&live, peer);
    assert_whole_history(
        kind,
        "scrolled down from the first unread message",
        &view.ids,
    );
}

/// The chat opens at the end, the user jumps to the first message (a reply to it, a link, the
/// calendar), then goes back to the end and scrolls up.
fn jump_to_the_first_message_then_scroll_up_from_the_end(kind: Kind) {
    let (_rpc, live, peer) = setup(kind);
    let first_open = View::open(&live, peer, MsgId::SHOW_AT_END);
    assert_eq!(first_open.ids.last(), Some(&TOTAL), "{kind:?}");

    let jump = live.load_slice_around(peer, MsgId(1), 40, 40);
    assert_eq!(
        jump.messages.first().map(|m| m.id.0),
        Some(1),
        "{kind:?}: the jump shows the first message: {:?}",
        jump.messages.iter().map(|m| m.id.0).collect::<Vec<_>>()
    );
    assert!(
        jump.messages.iter().all(|m| m.id.0 <= 41),
        "{kind:?}: the jump page holds only the messages around the first one"
    );
    assert!(
        !jump.loaded_at_bottom,
        "{kind:?}: the jump page is not the end"
    );

    let mut at_end = View::open(&live, peer, MsgId::SHOW_AT_END);
    at_end.scroll_to_top(&live, peer);
    assert_whole_history(
        kind,
        "scrolled up after a jump to the first message",
        &at_end.ids,
    );
}

/// A range at the top of the history loaded on its own earlier (the first messages, as a page
/// that reached the top leaves them): opening at the end and scrolling up must load the
/// messages between it and the bottom.
fn a_range_at_the_top_then_scroll_up_from_the_end(kind: Kind) {
    let (_rpc, live, peer) = setup(kind);
    live.write_session(|s| {
        let self_id = s.self_user_id();
        let first: Vec<_> = (1..=2)
            .filter_map(|id| tg_api::convert::message_from_tl(&kind.message(id), self_id))
            .collect();
        s.add_history_slice(
            kind.data_peer(),
            first,
            MsgRange::new(DataMsgId(0), DataMsgId(2)),
            Some(TOTAL),
        );
    });
    let mut at_end = View::open(&live, peer, MsgId::SHOW_AT_END);
    assert!(!at_end.at_top, "{kind:?}: the first page is not the top");
    at_end.scroll_to_top(&live, peer);
    assert_whole_history(
        kind,
        "scrolled up with the first messages loaded",
        &at_end.ids,
    );
}

#[test]
fn supergroup_scroll_up_from_the_end_after_opening_at_unread() {
    open_at_unread_then_scroll_up_from_the_end(Kind::Supergroup);
}

#[test]
fn basic_group_scroll_up_from_the_end_after_opening_at_unread() {
    open_at_unread_then_scroll_up_from_the_end(Kind::BasicGroup);
}

#[test]
fn private_chat_scroll_up_from_the_end_after_opening_at_unread() {
    open_at_unread_then_scroll_up_from_the_end(Kind::Private);
}

#[test]
fn supergroup_scroll_down_from_the_first_unread_message() {
    open_at_unread_then_scroll_down(Kind::Supergroup);
}

#[test]
fn basic_group_scroll_down_from_the_first_unread_message() {
    open_at_unread_then_scroll_down(Kind::BasicGroup);
}

#[test]
fn private_chat_scroll_down_from_the_first_unread_message() {
    open_at_unread_then_scroll_down(Kind::Private);
}

#[test]
fn supergroup_jump_to_the_first_message_then_scroll_up() {
    jump_to_the_first_message_then_scroll_up_from_the_end(Kind::Supergroup);
}

#[test]
fn private_chat_jump_to_the_first_message_then_scroll_up() {
    jump_to_the_first_message_then_scroll_up_from_the_end(Kind::Private);
}

#[test]
fn supergroup_range_at_the_top_then_scroll_up() {
    a_range_at_the_top_then_scroll_up_from_the_end(Kind::Supergroup);
}

#[test]
fn basic_group_range_at_the_top_then_scroll_up() {
    a_range_at_the_top_then_scroll_up_from_the_end(Kind::BasicGroup);
}

/// A page that fails to load is empty but not the top (the view used to take an empty page
/// for the top and never asked again); the server is not asked again on every frame, only
/// after a pause, and then the page arrives.
#[test]
fn a_failed_older_page_is_not_the_top_and_is_retried_later() {
    let kind = Kind::Supergroup;
    let (rpc, live, peer) = setup(kind);
    let view = View::open(&live, peer, MsgId::SHOW_AT_END);
    let first = *view.ids.first().expect("the first page");
    assert!(!view.at_top);

    rpc.set_handler(|_, _| None);
    let failed = ready(|| live.try_load_slice_before(peer, MsgId(first), SCROLL_PAGE));
    assert!(failed.messages.is_empty());
    assert!(!failed.loaded_at_top, "a failed page is not the top");
    let asked = rpc.calls::<tl_fns::messages::GetHistory>().len();
    for _ in 0..5 {
        let again = ready(|| live.try_load_slice_before(peer, MsgId(first), SCROLL_PAGE));
        assert!(again.messages.is_empty() && !again.loaded_at_top);
    }
    assert_eq!(
        rpc.calls::<tl_fns::messages::GetHistory>().len(),
        asked,
        "no new request right after a failure"
    );

    rpc.set_handler(move |ctor, bytes| {
        if ctor != tl_fns::messages::GetHistory::CONSTRUCTOR_ID {
            return None;
        }
        let req = tl_fns::messages::GetHistory::from_bytes(bytes).ok()?;
        Some(serve_page(kind, &req))
    });
    std::thread::sleep(Duration::from_millis(2100));
    let retried = ready(|| live.try_load_slice_before(peer, MsgId(first), SCROLL_PAGE));
    assert_eq!(
        retried.messages.len(),
        SCROLL_PAGE,
        "the page arrives on the retry"
    );
    assert_eq!(
        retried.messages.last().map(|m| m.id.0),
        Some(first - 1),
        "it continues right below the view"
    );
}

/// Opening at the end with only the dialog's last message known, then scrolling up, loads
/// the whole history (the case that already worked; it must keep working).
#[test]
fn supergroup_plain_scroll_up_from_the_end() {
    let (_rpc, live, peer) = setup(Kind::Supergroup);
    let mut view = View::open(&live, peer, MsgId::SHOW_AT_END);
    view.scroll_to_top(&live, peer);
    assert_whole_history(Kind::Supergroup, "plain scroll up", &view.ids);
}
