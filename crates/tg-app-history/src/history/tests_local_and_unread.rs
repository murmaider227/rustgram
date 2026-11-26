//! Local (pending / failed) messages in the list (TGB-2931), the first page loaded without
//! waiting (TGB-2926) and opening at the "Unread messages" bar (TGB-2902).

use egui::{Pos2, Rect, Vec2};
use egui_kittest::HarnessBuilder;
use tg_text::TextWithEntities;
use tg_ui::fonts::register_tdesktop_fonts;

use crate::history::backend::{HistoryMessage, HistoryPeerInfo, HistorySlice, MessageExtra};
use crate::history::inner::HistoryInnerWidget;
use crate::history::mock::MockHistoryStore;
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind, PeerSummary};

const BASE_UNIX: i32 = 1_795_340_000;

fn peer_info(peer: PeerId) -> HistoryPeerInfo {
    HistoryPeerInfo {
        summary: PeerSummary {
            id: peer,
            kind: PeerKind::User,
            title: "Ada North".to_owned(),
            status: "online".to_owned(),
            username: None,
            phone_or_link: None,
            about: String::new(),
            initials: "AN".to_owned(),
            avatar_rgb: [0x4B, 0x8A, 0xE6],
            muted: false,
            can_send: true,
            is_saved_messages: false,
        },
        typing_text: None,
        contact_status: None,
        unread_count: 0,
        can_pin: true,
        can_delete: true,
        link_base: String::new(),
    }
}

fn message(peer: PeerId, id: MsgId, outgoing: bool, text: &str) -> HistoryMessage {
    HistoryMessage {
        id,
        peer_id: peer,
        from_id: peer,
        sender_name: "Ada North".to_owned(),
        date_unix: BASE_UNIX + id.0.rem_euclid(10_000) * 60,
        outgoing,
        is_post: false,
        text: TextWithEntities::simple(text),
        media: None,
        service: None,
        reply: None,
        forward: None,
        reactions: Vec::new(),
        views: None,
        edited: false,
        is_read: true,
        is_sending: false,
        is_pinned: false,
        extra: MessageExtra::default(),
    }
}

fn local(index: i32) -> MsgId {
    MsgId::local((1 << 29) + index).expect("local id")
}

fn pending(peer: PeerId, id: MsgId, text: &str) -> HistoryMessage {
    HistoryMessage {
        is_sending: true,
        is_read: false,
        date_unix: BASE_UNIX + 10_000 * 60,
        ..message(peer, id, true, text)
    }
}

fn opened(peer: PeerId, ids: std::ops::RangeInclusive<i32>) -> HistoryInnerWidget {
    let mut inner = HistoryInnerWidget::new();
    inner.apply_slice(
        peer,
        HistorySlice {
            messages: ids
                .map(|i| message(peer, MsgId(i), false, "earlier"))
                .collect(),
            loaded_at_top: true,
            loaded_at_bottom: true,
            ..HistorySlice::default()
        },
        PeerKind::User,
        true,
    );
    inner
}

fn ids(inner: &HistoryInnerWidget) -> Vec<MsgId> {
    inner.messages().iter().map(|m| m.id).collect()
}

#[test]
fn a_sent_local_message_is_swapped_in_place_keeping_its_row() {
    let peer = PeerId::from_user(91_200);
    let mut inner = opened(peer, 101..=103);
    let first = local(0);
    inner.append_message(pending(peer, first, "one"), PeerKind::User);
    // An incoming message arrives while "one" is still being sent: it goes after it.
    inner.apply_live_added_message(message(peer, MsgId(104), false, "reply"), PeerKind::User);
    let second = local(1);
    inner.apply_live_added_message(pending(peer, second, "two"), PeerKind::User);
    assert_eq!(
        ids(&inner),
        vec![
            MsgId(101),
            MsgId(102),
            MsgId(103),
            first,
            MsgId(104),
            second
        ]
    );
    let row_before = inner.list().items()[3].clone();

    let sent = message(peer, MsgId(105), true, "one");
    assert!(inner.replace_local_message(first, sent, PeerKind::User));
    assert_eq!(
        ids(&inner),
        vec![
            MsgId(101),
            MsgId(102),
            MsgId(103),
            MsgId(105),
            MsgId(104),
            second
        ],
        "the sent message keeps the pending message's place"
    );
    let row_after = &inner.list().items()[3];
    assert_eq!(row_after.y, row_before.y);
    assert_eq!(row_after.height, row_before.height);
    assert!(!inner.messages()[3].is_sending);
}

#[test]
fn a_sent_message_already_listed_by_a_reload_drops_the_local_copy() {
    let peer = PeerId::from_user(91_201);
    let mut inner = opened(peer, 101..=102);
    let first = local(0);
    inner.append_message(pending(peer, first, "one"), PeerKind::User);
    inner.apply_live_added_message(message(peer, MsgId(103), true, "one"), PeerKind::User);
    inner.replace_local_message(
        first,
        message(peer, MsgId(103), true, "one"),
        PeerKind::User,
    );
    assert_eq!(ids(&inner), vec![MsgId(101), MsgId(102), MsgId(103)]);
}

#[test]
fn a_failed_local_message_keeps_the_clock() {
    let peer = PeerId::from_user(91_202);
    let mut inner = opened(peer, 101..=101);
    let first = local(0);
    inner.append_message(pending(peer, first, "lost"), PeerKind::User);
    assert!(inner.mark_local_message_failed(first));
    let msg = inner.messages().last().expect("failed message stays");
    assert_eq!(msg.id, first);
    assert!(msg.is_sending, "the clock stays (BottomInfo Flag::Sending)");
    assert!(msg.extra.send_failed);
}

#[test]
fn streamed_drafts_never_remove_local_messages() {
    let peer = PeerId::from_user(91_203);
    let mut inner = opened(peer, 101..=101);
    let first = local(0);
    inner.append_message(pending(peer, first, "pending"), PeerKind::User);
    let draft = message(peer, MsgId(-100_000), false, "bot is typing");
    inner.sync_streamed_drafts(vec![draft], PeerKind::User);
    assert_eq!(ids(&inner), vec![MsgId(101), first, MsgId(-100_000)]);
    inner.sync_streamed_drafts(Vec::new(), PeerKind::User);
    assert_eq!(
        ids(&inner),
        vec![MsgId(101), first],
        "only the draft goes when the draft list empties"
    );
}

#[test]
fn an_older_server_message_goes_before_the_local_ones() {
    let peer = PeerId::from_user(91_204);
    let mut inner = opened(peer, 101..=101);
    inner.apply_live_added_message(message(peer, MsgId(105), false, "new"), PeerKind::User);
    let first = local(0);
    inner.apply_live_added_message(pending(peer, first, "pending"), PeerKind::User);
    inner.apply_live_added_message(message(peer, MsgId(103), false, "late"), PeerKind::User);
    assert_eq!(ids(&inner), vec![MsgId(101), MsgId(103), MsgId(105), first]);
}

#[test]
fn the_first_page_loads_without_waiting_and_keeps_its_anchor() {
    let peer = PeerId::from_user(91_205);
    let mut store = MockHistoryStore::new();
    for i in 0..80 {
        store.push_message(peer, message(peer, MsgId(100 + i), false, "msg"));
    }
    store.set_first_unread(peer, Some(MsgId(150)));
    store.set_first_load_pending(peer, true);

    let mut inner = opened(PeerId::from_user(91_999), 1..=3);
    inner.load_peer(peer, MsgId::SHOW_AT_UNREAD, &store, PeerKind::User);
    assert!(inner.is_loading_peer());
    assert_eq!(inner.peer_id(), peer);
    assert!(
        inner.messages().is_empty(),
        "the previous chat's messages are gone, nothing else is shown yet"
    );

    inner.poll_pending_open(&store);
    assert!(inner.is_loading_peer(), "still waiting for the page");

    store.set_first_load_pending(peer, false);
    inner.poll_pending_open(&store);
    assert!(!inner.is_loading_peer());
    assert!(
        inner.messages().iter().any(|m| m.id == MsgId(150)),
        "the page around the first unread message arrived"
    );
    let bar = inner.list().unread_bar_index.expect("unread bar");
    assert_eq!(inner.messages()[bar].id, MsgId(150));
}

struct UnreadState {
    inner: HistoryInnerWidget,
    store: MockHistoryStore,
    peer_info: HistoryPeerInfo,
    list_rect: Rect,
    colors: HistoryColors,
}

fn open_and_render(first_unread: Option<MsgId>) -> (Rect, Option<Rect>, Option<Rect>) {
    let peer = PeerId::from_user(91_206);
    let list_rect = Rect::from_min_max(Pos2::new(470.0, 106.0), Pos2::new(1172.0, 900.0));
    let mut store = MockHistoryStore::new();
    for i in 0..90 {
        store.push_message(
            peer,
            message(
                peer,
                MsgId(100 + i),
                i % 2 == 0,
                "A message with enough words to wrap over a couple of lines in the list.",
            ),
        );
    }
    store.set_first_unread(peer, first_unread);
    let mut inner = HistoryInnerWidget::new();
    inner.load_peer(peer, MsgId::SHOW_AT_UNREAD, &store, PeerKind::User);
    let state = UnreadState {
        inner,
        store,
        peer_info: peer_info(peer),
        list_rect,
        colors: HistoryColors::from_palette(&Palette::for_mode(ThemeMode::Day)),
    };
    let mut harness = HarnessBuilder::default()
        .with_size(Vec2::new(1180.0, 960.0))
        .with_step_dt(0.05)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(register_tdesktop_fonts)
        .build_ui_state(
            |ui, st: &mut UnreadState| {
                let _ = st.inner.show(
                    ui,
                    st.list_rect,
                    &st.peer_info,
                    false,
                    "",
                    &st.colors,
                    &mut st.store,
                );
            },
            state,
        );
    for _ in 0..6 {
        harness.step();
    }
    let st = harness.state();
    let unread = first_unread.and_then(|id| st.inner.last_painted_bubble_rect(id));
    let last = st
        .inner
        .messages()
        .last()
        .and_then(|m| st.inner.last_painted_bubble_rect(m.id));
    (list_rect, unread, last)
}

#[test]
fn a_chat_with_unread_messages_opens_with_the_unread_bar_at_the_top() {
    let (list_rect, unread, last) = open_and_render(Some(MsgId(140)));
    let unread = unread.expect("the first unread message is on screen");
    assert!(
        unread.top() >= list_rect.top() && unread.top() <= list_rect.top() + 90.0,
        "first unread bubble top {} must sit just below the bar at the list top {}",
        unread.top(),
        list_rect.top()
    );
    assert!(
        last.is_none(),
        "the newest message (50 messages below) is not on screen"
    );
}

#[test]
fn a_chat_without_unread_messages_opens_at_the_bottom() {
    let (list_rect, _, last) = open_and_render(None);
    let last = last.expect("the newest message is on screen");
    assert!(
        last.bottom() <= list_rect.bottom() && last.bottom() >= list_rect.bottom() - 80.0,
        "newest bubble bottom {} must sit at the list bottom {}",
        last.bottom(),
        list_rect.bottom()
    );
}
