//! TGB-3161 (walk OB-06 / OB-07): scrolled up in a group, new messages show their count on the
//! scroll-down corner button and a new mention shows the "@" corner button with its counter
//! (`HistoryWidget::unreadCountUpdated`, `CornerButtons::updateJumpDownVisibility` /
//! `updateUnreadThingsVisibility`, `history_view_corner_buttons.cpp:460-527`).

use egui::Context;
use tg_app_core::core_types::dto::engagement::{UnreadThingsKind, UnreadThingsScope};
use tg_app_core::history::backend::HistoryBackend;
use tg_app_core::stand_in::types::MsgId;
use tg_app_live::session::LiveSession;
use tg_tl::{Identifiable, enums as tl_enums, functions as tl_fns, types as tl_types};

use super::tests_wp79_live_history::{
    FakeRpcClient, render_frame, start_live_fixture, tl_text_message, wait_for_generation_above,
};
use crate::history::HistoryWidget;

const GROUP_ID: i64 = 601;
const SENDER_ID: i64 = 301;

fn group_message(id: i32, text: &str, mentions_me: bool) -> tl_enums::Message {
    let mut msg = tl_text_message(
        id,
        tl_enums::Peer::Channel(tl_types::PeerChannel {
            channel_id: GROUP_ID,
        }),
        Some(tl_enums::Peer::User(tl_types::PeerUser {
            user_id: SENDER_ID,
        })),
        false,
        1_795_341_000 + id * 10,
        text,
        None,
    );
    if let tl_enums::Message::Message(m) = &mut msg {
        m.mentioned = mentions_me;
        m.media_unread = mentions_me;
    }
    msg
}

/// Queues the first page of the fixture's megagroup: 30 read messages.
pub(super) fn open_group_of_30(rpc: &FakeRpcClient) {
    let page: Vec<tl_enums::Message> = (1..=30)
        .rev()
        .map(|id| {
            group_message(
                id,
                &format!("Group message #{id} with enough text to fill a bubble line"),
                false,
            )
        })
        .collect();
    rpc.enqueue_ok(
        tl_fns::messages::GetHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::ChannelMessages(tl_types::messages::ChannelMessages {
            inexact: false,
            pts: 100,
            offset_id_offset: None,
            count: 30,
            messages: page,
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
}

/// Pushes `updateNewChannelMessage` #`id` (after #30) and waits until the session has it.
pub(super) fn deliver(rpc: &FakeRpcClient, live: &LiveSession, id: i32, mentions_me: bool) {
    let gen_before = live.bridge_state().generation();
    rpc.push_update(tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::NewChannelMessage(
            tl_types::UpdateNewChannelMessage {
                message: group_message(id, &format!("New message #{id}"), mentions_me),
                pts: 100 + id - 30,
                pts_count: 1,
            },
        )],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_342_000 + id,
        seq: 0,
    }));
    wait_for_generation_above(live, gen_before);
}

#[test]
fn scrolled_up_group_counts_new_messages_and_shows_the_mention_button() {
    let (_rt, rpc, mut live, _peer_a, group) = start_live_fixture(100);

    // A 30-message group, opened at the bottom with everything read.
    open_group_of_30(&rpc);
    let ctx = Context::default();
    let mut widget = HistoryWidget::new();
    render_frame(&ctx, &mut widget, &mut live, group, 1.0, true);
    render_frame(&ctx, &mut widget, &mut live, group, 1.05, true);
    assert_eq!(widget.inner_mut().messages().len(), 30);

    // Scroll up to the first message.
    widget.inner_mut().set_scroll_offset(0.0);
    render_frame(&ctx, &mut widget, &mut live, group, 1.1, true);
    render_frame(&ctx, &mut widget, &mut live, group, 1.15, true);
    assert!(widget.inner_mut().show_scroll_down_button());

    // Three new messages arrive; the second one mentions the user.
    for (id, mentions_me) in [(31, false), (32, true), (33, false)] {
        deliver(&rpc, &live, id, mentions_me);
        render_frame(
            &ctx,
            &mut widget,
            &mut live,
            group,
            2.0 + f64::from(id) * 0.1,
            true,
        );
    }
    for step in 0..4 {
        render_frame(
            &ctx,
            &mut widget,
            &mut live,
            group,
            6.0 + f64::from(step) * 0.2,
            true,
        );
    }

    assert!(
        widget.inner_mut().show_scroll_down_button(),
        "the new messages must not scroll the view down"
    );
    assert_eq!(
        widget.inner_mut().unread_count(),
        3,
        "the scroll-down button counts the three new messages"
    );
    let scope = UnreadThingsScope::history(group);
    assert_eq!(live.unread_things_counts(scope).mentions, 1);
    assert!(
        widget
            .unread_things_mut()
            .button_rect(UnreadThingsKind::Mentions)
            .is_some(),
        "the \"@\" button shows while the mention is below the view"
    );
    assert!(
        widget
            .inner_mut()
            .last_painted_bubble_rect(MsgId(32))
            .is_none()
    );
}

/// While the session does not know the chat's unread count it sends no `UnreadChanged`: the
/// list's own counter must count every unread arrival (`History::addNewMessage` adds one).
#[test]
fn each_unread_arrival_counts_without_a_session_count() {
    use crate::history::backend::{HistoryMessage, HistorySlice, MessageExtra};
    use tg_app_core::stand_in::types::{PeerId, PeerKind};

    let peer = PeerId::from_chat(3161);
    let message = |id: i32, is_read: bool| HistoryMessage {
        id: MsgId(id),
        peer_id: peer,
        from_id: PeerId::from_user(301),
        sender_name: "Sample Sender".to_owned(),
        date_unix: 1_795_341_000 + id * 10,
        outgoing: false,
        is_post: false,
        text: tg_text::TextWithEntities::simple("hello"),
        media: None,
        service: None,
        reply: None,
        forward: None,
        reactions: Vec::new(),
        views: None,
        edited: false,
        is_read,
        is_sending: false,
        is_pinned: false,
        extra: MessageExtra::default(),
    };
    let mut inner = crate::history::inner::HistoryInnerWidget::new();
    inner.apply_slice(
        peer,
        HistorySlice {
            messages: (1..=5).map(|id| message(id, true)).collect(),
            loaded_at_top: true,
            loaded_at_bottom: true,
            ..HistorySlice::default()
        },
        PeerKind::Group,
        true,
    );
    for id in 6..=8 {
        inner.apply_live_added_message(message(id, false), PeerKind::Group);
    }
    assert_eq!(inner.unread_count(), 3);
}

/// After "Mark all as read" on the "@" button a new mention brings it back: the count is the
/// thread's one counter (`thread->unreadMentions().count()`), not a value kept from the read.
#[test]
fn a_mention_after_marking_all_read_shows_the_button_again() {
    let (_rt, rpc, mut live, _peer_a, group) = start_live_fixture(100);
    open_group_of_30(&rpc);
    let ctx = Context::default();
    let mut widget = HistoryWidget::new();
    render_frame(&ctx, &mut widget, &mut live, group, 1.0, true);
    render_frame(&ctx, &mut widget, &mut live, group, 1.05, true);
    widget.inner_mut().set_scroll_offset(0.0);
    render_frame(&ctx, &mut widget, &mut live, group, 1.1, true);

    let scope = UnreadThingsScope::history(group);
    deliver(&rpc, &live, 31, true);
    assert_eq!(live.unread_things_counts(scope).mentions, 1);

    rpc.enqueue_ok(
        tl_fns::messages::ReadMentions::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedHistory::AffectedHistory(
            tl_types::messages::AffectedHistory {
                pts: 101,
                pts_count: 0,
                offset: 0,
            },
        ),
    );
    live.read_unread_mentions(group, None)
        .expect("messages.readMentions");
    assert_eq!(live.unread_things_counts(scope).mentions, 0);

    deliver(&rpc, &live, 32, true);
    assert_eq!(
        live.unread_things_counts(scope).mentions,
        1,
        "the new mention counts after the earlier ones were read"
    );
    for step in 0..3 {
        render_frame(
            &ctx,
            &mut widget,
            &mut live,
            group,
            3.0 + f64::from(step) * 0.2,
            true,
        );
    }
    assert!(
        widget
            .unread_things_mut()
            .button_rect(UnreadThingsKind::Mentions)
            .is_some()
    );
}
