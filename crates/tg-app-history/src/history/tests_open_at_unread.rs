//! A chat with unread messages opens with the "Unread messages" bar at the top of the view
//! (`ShowAtUnreadMsgId`, `HistoryWidget::countInitialScrollTop` → `unreadBarTop`); a chat
//! without unread messages opens at the bottom.

use egui::{Pos2, Rect, Vec2};
use egui_kittest::{Harness, HarnessBuilder};
use tg_text::TextWithEntities;
use tg_ui::fonts::register_tdesktop_fonts;

use crate::history::backend::{HistoryMessage, HistoryPeerInfo, HistorySlice, MessageExtra};
use crate::history::inner::{HistoryInnerWidget, InnerEvent};
use crate::history::mock::MockHistoryStore;
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind, PeerSummary};

const BASE_UNIX: i32 = 1_795_340_000;
const COUNT: i32 = 80;
/// The first unread message: 40 unread incoming messages follow it, far more than a screen.
const FIRST_UNREAD: i32 = 40;

struct State {
    inner: HistoryInnerWidget,
    store: MockHistoryStore,
    peer_info: HistoryPeerInfo,
    list_rect: Rect,
    colors: HistoryColors,
    events: Vec<InnerEvent>,
}

fn peer_info(peer: PeerId, unread: u32) -> HistoryPeerInfo {
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
        unread_count: unread,
        can_pin: true,
        can_delete: true,
        link_base: String::new(),
    }
}

fn message(peer: PeerId, index: i32, unread: bool) -> HistoryMessage {
    HistoryMessage {
        id: MsgId(100 + index),
        peer_id: peer,
        from_id: peer,
        sender_name: "Ada North".to_owned(),
        date_unix: BASE_UNIX + index * 60,
        outgoing: false,
        is_post: false,
        text: TextWithEntities::simple(format!("Incoming message number {index}.")),
        media: None,
        service: None,
        reply: None,
        forward: None,
        reactions: Vec::new(),
        views: None,
        edited: false,
        is_read: !unread,
        is_sending: false,
        is_pinned: false,
        extra: MessageExtra::default(),
    }
}

fn open(unread: bool) -> (Harness<'static, State>, Rect) {
    let peer = PeerId::from_user(91_200);
    let list_rect = Rect::from_min_max(Pos2::new(470.0, 106.0), Pos2::new(1172.0, 806.0));
    let unread_count = if unread {
        (COUNT - FIRST_UNREAD) as u32
    } else {
        0
    };
    let messages: Vec<HistoryMessage> = (0..COUNT)
        .map(|i| message(peer, i, unread && i >= FIRST_UNREAD))
        .collect();
    let mut store = MockHistoryStore::new();
    for m in &messages {
        store.push_message(peer, m.clone());
    }
    let mut inner = HistoryInnerWidget::new();
    // What `load_peer(peer, MsgId::SHOW_AT_UNREAD, ..)` receives from the backend.
    inner.apply_slice(
        peer,
        HistorySlice {
            messages,
            loaded_at_top: true,
            loaded_at_bottom: true,
            unread_count,
            first_unread_id: unread.then_some(MsgId(100 + FIRST_UNREAD)),
            total_count: COUNT as usize,
        },
        PeerKind::User,
        false,
    );
    assert_eq!(inner.opens_at_unread_bar(), unread);
    let state = State {
        inner,
        store,
        peer_info: peer_info(peer, unread_count),
        list_rect,
        colors: HistoryColors::from_palette(&Palette::for_mode(ThemeMode::Day)),
        events: Vec::new(),
    };
    let harness = HarnessBuilder::default()
        .with_size(Vec2::new(1180.0, 860.0))
        .with_step_dt(0.05)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(register_tdesktop_fonts)
        .build_ui_state(
            |ui, st: &mut State| {
                let evs = st.inner.show(
                    ui,
                    st.list_rect,
                    &st.peer_info,
                    false,
                    "",
                    &st.colors,
                    &mut st.store,
                );
                st.events.extend(evs);
            },
            state,
        );
    (harness, list_rect)
}

#[test]
fn unread_chat_opens_with_the_unread_bar_at_the_top() {
    let (mut harness, list_rect) = open(true);
    for _ in 0..10 {
        harness.step();
    }
    let st = harness.state();
    let first_unread = st
        .inner
        .last_painted_bubble_rect(MsgId(100 + FIRST_UNREAD))
        .expect("the first unread message is on screen");
    // The bar (about 30 px) sits at the top; the first unread bubble starts right below it.
    assert!(
        first_unread.top() >= list_rect.top() && first_unread.top() <= list_rect.top() + 60.0,
        "first unread bubble top {} must sit just below the bar at the list top {}",
        first_unread.top(),
        list_rect.top()
    );
    assert!(
        st.inner
            .last_painted_bubble_rect(MsgId(100 + COUNT - 1))
            .is_none(),
        "the newest message is below the screen"
    );
    assert!(
        st.inner.unread_count() > 0,
        "nothing below the screen is read yet"
    );
}

#[test]
fn chat_without_unread_opens_at_the_bottom() {
    let (mut harness, list_rect) = open(false);
    for _ in 0..10 {
        harness.step();
    }
    let newest = harness
        .state()
        .inner
        .last_painted_bubble_rect(MsgId(100 + COUNT - 1))
        .expect("the newest message is on screen");
    assert!(
        newest.bottom() <= list_rect.bottom() && newest.bottom() >= list_rect.bottom() - 80.0,
        "newest bubble bottom {} must sit at the list bottom {}",
        newest.bottom(),
        list_rect.bottom()
    );
}
