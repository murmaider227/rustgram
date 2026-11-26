//! A chat opened with only its newest message loaded (the dialog's top message) fills older
//! messages in pages while the view stays at the newest message, like tdesktop's
//! `HistoryWidget::updateHistoryGeometry` keeping a view that was at the bottom there.

use egui::{Pos2, Rect, Vec2};
use egui_kittest::HarnessBuilder;
use tg_text::TextWithEntities;
use tg_ui::fonts::register_tdesktop_fonts;

use crate::history::backend::{
    HistoryMedia, HistoryMessage, HistoryPeerInfo, HistorySlice, MessageExtra, PhotoAttachment,
};
use crate::history::inner::{HistoryInnerWidget, InnerEvent};
use crate::history::mock::MockHistoryStore;
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind, PeerSummary};

const BASE_UNIX: i32 = 1_795_340_000;
const COUNT: i32 = 90;

struct State {
    inner: HistoryInnerWidget,
    store: MockHistoryStore,
    peer_info: HistoryPeerInfo,
    list_rect: Rect,
    colors: HistoryColors,
    events: Vec<InnerEvent>,
}

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

/// Every third message is a tall photo whose real height differs a lot from the estimate.
fn message(peer: PeerId, index: i32) -> HistoryMessage {
    let media = (index % 3 == 0).then(|| {
        HistoryMedia::Photo(PhotoAttachment {
            width: 840,
            height: 1050,
            stripped_thumb: None,
            fallback_rgb: ([0x4B, 0x8A, 0xE6], [0x1C, 0x2F, 0x4E]),
            file_name: format!("photo_{index}.png"),
            size_bytes: 192_400,
        })
    });
    HistoryMessage {
        id: MsgId(100 + index),
        peer_id: peer,
        from_id: peer,
        sender_name: "Ada North".to_owned(),
        date_unix: BASE_UNIX + index * 60,
        outgoing: index % 2 == 0,
        is_post: false,
        text: TextWithEntities::simple(format!(
            "Message number {index} with enough words to wrap over a couple of lines here."
        )),
        media,
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

#[test]
fn chat_opened_with_only_its_top_message_stays_at_the_newest_message_while_older_ones_load() {
    let peer = PeerId::from_user(91_100);
    let list_rect = Rect::from_min_max(Pos2::new(470.0, 106.0), Pos2::new(1172.0, 1224.0));
    let mut store = MockHistoryStore::new();
    for index in 0..COUNT {
        store.push_message(peer, message(peer, index));
    }
    let last_id = MsgId(100 + COUNT - 1);

    let mut inner = HistoryInnerWidget::new();
    inner.apply_slice(
        peer,
        HistorySlice {
            messages: vec![message(peer, COUNT - 1)],
            loaded_at_top: false,
            loaded_at_bottom: true,
            unread_count: 0,
            first_unread_id: None,
            total_count: COUNT as usize,
        },
        PeerKind::User,
        true,
    );
    let state = State {
        inner,
        store,
        peer_info: peer_info(peer),
        list_rect,
        colors: HistoryColors::from_palette(&Palette::for_mode(ThemeMode::Day)),
        events: Vec::new(),
    };
    let mut harness = HarnessBuilder::default()
        .with_size(Vec2::new(1180.0, 1280.0))
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

    for frame in 1..=40 {
        harness.step();
        let st = harness.state();
        let bubble = st
            .inner
            .last_painted_bubble_rect(last_id)
            .unwrap_or_else(|| {
                panic!(
                    "frame {frame}: the newest message must stay on screen ({} loaded)",
                    st.inner.messages().len()
                )
            });
        assert!(
            bubble.bottom() <= list_rect.bottom() && bubble.bottom() >= list_rect.bottom() - 80.0,
            "frame {frame}: newest bubble bottom {} must sit at the list bottom {}",
            bubble.bottom(),
            list_rect.bottom()
        );
    }
    assert!(
        harness.state().inner.messages().len() > 1,
        "older messages must have been loaded while scrolling stays at the bottom"
    );
}
