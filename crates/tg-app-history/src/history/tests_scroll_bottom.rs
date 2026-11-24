//! Regression tests ensuring `HistoryInnerWidget` scrolls all the way to the bottom when
//! loaded at `MsgId::SHOW_AT_END` or when new messages are appended, so link-preview and
//! paid-media bubbles at the end of history stay completely inside `list_rect`.

use egui::{Pos2, Rect, Vec2};
use egui_kittest::HarnessBuilder;
use egui_kittest::kittest::Queryable;
use tg_text::TextWithEntities;
use tg_ui::fonts::register_tdesktop_fonts;

use crate::history::backend::{
    HistoryMedia, HistoryMessage, HistoryPeerInfo, HistorySlice, MessageExtra, PhotoAttachment,
    PollAttachment, PollOption,
};
use crate::history::inner::{HistoryInnerWidget, InnerEvent};
use crate::history::mock::MockHistoryStore;
use crate::history::sections_model::LinkPreviewData;
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind, PeerSummary};

const BASE_UNIX: i32 = 1_795_340_000;

struct ScrollBottomTestState {
    inner: HistoryInnerWidget,
    store: MockHistoryStore,
    peer_info: HistoryPeerInfo,
    list_rect: Rect,
    colors: HistoryColors,
    events: Vec<InnerEvent>,
}

fn make_peer_info(peer: PeerId) -> HistoryPeerInfo {
    HistoryPeerInfo {
        summary: PeerSummary {
            id: peer,
            kind: PeerKind::User,
            title: "Mira Vance".to_owned(),
            status: "online".to_owned(),
            username: Some("mira_vance".to_owned()),
            phone_or_link: None,
            about: String::new(),
            initials: "MV".to_owned(),
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
        link_base: "https://t.me/mira_vance".to_owned(),
    }
}

fn make_tall_prefix_messages(peer: PeerId) -> Vec<HistoryMessage> {
    vec![
        HistoryMessage {
            id: MsgId(201),
            peer_id: peer,
            from_id: peer,
            sender_name: "Mira Vance".to_owned(),
            date_unix: BASE_UNIX,
            outgoing: false,
            is_post: false,
            text: TextWithEntities::simple(
                "Here is the studio poster render — tap it to test zoom & rotate in Media Viewer.",
            ),
            media: Some(HistoryMedia::Photo(PhotoAttachment {
                width: 840,
                height: 1050,
                stripped_thumb: None,
                fallback_rgb: ([0x4B, 0x8A, 0xE6], [0x1C, 0x2F, 0x4E]),
                file_name: "studio_grid_poster.png".to_owned(),
                size_bytes: 192_400,
            })),
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
        },
        HistoryMessage {
            id: MsgId(2011),
            peer_id: peer,
            from_id: peer,
            sender_name: "Mira Vance".to_owned(),
            date_unix: BASE_UNIX + 120,
            outgoing: false,
            is_post: false,
            text: TextWithEntities::default(),
            media: Some(HistoryMedia::Poll(PollAttachment {
                question: "Pick a sticker pack accent theme:".to_owned(),
                is_quiz: false,
                explanation: None,
                is_anonymous: false,
                multiple_choice: false,
                closed: false,
                options: vec![
                    PollOption {
                        text: "Classic Day Blue".to_owned(),
                        votes: 6,
                        chosen: false,
                        correct: false,
                    },
                    PollOption {
                        text: "Warm Sunset Coral".to_owned(),
                        votes: 3,
                        chosen: false,
                        correct: false,
                    },
                ],
                total_voters: 9,
            })),
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
        },
        HistoryMessage {
            id: MsgId(2012),
            peer_id: peer,
            from_id: peer,
            sender_name: "Mira Vance".to_owned(),
            date_unix: BASE_UNIX + 210,
            outgoing: false,
            is_post: false,
            text: TextWithEntities::simple("Quick design preview."),
            media: Some(HistoryMedia::Photo(PhotoAttachment {
                width: 960,
                height: 640,
                stripped_thumb: None,
                fallback_rgb: ([0x30, 0x62, 0x98], [0x6A, 0xB4, 0xEB]),
                file_name: "design_preview.jpg".to_owned(),
                size_bytes: 128_000,
            })),
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
        },
        HistoryMessage {
            id: MsgId(202),
            peer_id: peer,
            from_id: peer,
            sender_name: "Mira Vance".to_owned(),
            date_unix: BASE_UNIX + 300,
            outgoing: false,
            is_post: false,
            text: TextWithEntities::simple(
                "Can you verify back navigation from Info -> History and emoji insertion?",
            ),
            media: None,
            service: None,
            reply: None,
            forward: None,
            reactions: Vec::new(),
            views: None,
            edited: false,
            is_read: false,
            is_sending: false,
            is_pinned: false,
            extra: MessageExtra::default(),
        },
    ]
}

fn make_link_preview_message(
    peer: PeerId,
    id: MsgId,
    date_unix: i32,
    text: &str,
    lp: LinkPreviewData,
) -> HistoryMessage {
    HistoryMessage {
        id,
        peer_id: peer,
        from_id: peer,
        sender_name: "Mira Vance".to_owned(),
        date_unix,
        outgoing: false,
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
        extra: MessageExtra {
            link_preview: Some(lp),
            ..MessageExtra::default()
        },
    }
}

fn run_scroll_bottom_harness(
    peer: PeerId,
    messages: Vec<HistoryMessage>,
    list_rect: Rect,
) -> egui_kittest::Harness<'static, ScrollBottomTestState> {
    let mut inner = HistoryInnerWidget::new();
    let total_count = messages.len();
    let slice = HistorySlice {
        messages,
        loaded_at_top: true,
        loaded_at_bottom: true,
        unread_count: 1,
        first_unread_id: Some(MsgId(202)),
        total_count,
    };
    inner.apply_slice(peer, slice, PeerKind::User, true);

    let state = ScrollBottomTestState {
        inner,
        store: MockHistoryStore::new(),
        peer_info: make_peer_info(peer),
        list_rect,
        colors: HistoryColors::from_palette(&Palette::for_mode(ThemeMode::Day)),
        events: Vec::new(),
    };

    HarnessBuilder::default()
        .with_size(Vec2::new(1180.0, 1280.0))
        .with_step_dt(0.05)
        .with_max_steps(8)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
        })
        .build_ui_state(
            |ui, st: &mut ScrollBottomTestState| {
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
        )
}

#[test]
fn last_link_preview_and_paid_media_bubbles_stay_above_list_bottom() {
    let list_rect = Rect::from_min_max(Pos2::new(470.0, 106.0), Pos2::new(1172.0, 1224.0));

    // 1. Non-paid message with a standard link preview at the end of a scrollable history.
    {
        let peer = PeerId::from_user(91_001);
        let mut msgs = make_tall_prefix_messages(peer);
        let lp_id = MsgId(901);
        msgs.push(make_link_preview_message(
            peer,
            lp_id,
            BASE_UNIX + 90_000,
            "Check out the full release notes here: https://example.org/release",
            LinkPreviewData {
                url: "https://example.org/release".to_owned(),
                site_name: "Example Docs".to_owned(),
                title: "Rustgram Release Notes".to_owned(),
                description: "Scroll anchoring and virtualized layout updates.".to_owned(),
                thumb_rgb: None,
                large_media: false,
                above_text: false,
            },
        ));

        let mut harness = run_scroll_bottom_harness(peer, msgs, list_rect);
        for frame in 1..=12 {
            harness.step();
            let bubble = harness
                .state()
                .inner
                .last_painted_bubble_rect(lp_id)
                .unwrap_or_else(|| {
                    panic!("non-paid link-preview bubble must be painted on frame {frame}")
                });
            assert!(
                bubble.bottom() <= list_rect.bottom(),
                "frame {frame}: non-paid link-preview bubble bottom ({}) exceeded list_rect.bottom() ({})",
                bubble.bottom(),
                list_rect.bottom()
            );
            assert!(
                bubble.top() >= list_rect.top(),
                "frame {frame}: non-paid link-preview bubble top ({}) was above list_rect.top() ({})",
                bubble.top(),
                list_rect.top()
            );
        }
    }

    // 2. Paid-media message (after an invoice bubble) at the end of a scrollable history,
    //    plus appending another paid-media bubble while already at bottom.
    {
        let peer = PeerId::from_user(91_002);
        let mut msgs = make_tall_prefix_messages(peer);
        let inv_id = MsgId(901);
        let paid_id = MsgId(902);
        msgs.push(make_link_preview_message(
            peer,
            inv_id,
            BASE_UNIX + 90_000,
            "Pro License",
            LinkPreviewData {
                url: "tg://invoice?slug=bubble_inv_1".to_owned(),
                site_name: "Invoice".to_owned(),
                title: "Pro License — $5.00".to_owned(),
                description: "Annual plan".to_owned(),
                thumb_rgb: None,
                large_media: false,
                above_text: false,
            },
        ));
        msgs.push(make_link_preview_message(
            peer,
            paid_id,
            BASE_UNIX + 90_010,
            "Exclusive drop",
            LinkPreviewData {
                url: format!("paid_media:{}:902:50", peer.0),
                site_name: "Paid Media".to_owned(),
                title: "Unlock for 50 ⭐".to_owned(),
                description: "1 paid media item(s)".to_owned(),
                thumb_rgb: None,
                large_media: false,
                above_text: false,
            },
        ));

        let mut harness = run_scroll_bottom_harness(peer, msgs, list_rect);
        for frame in 1..=12 {
            harness.step();
            let bubble = harness
                .state()
                .inner
                .last_painted_bubble_rect(paid_id)
                .unwrap_or_else(|| panic!("paid-media bubble must be painted on frame {frame}"));
            assert!(
                bubble.bottom() <= list_rect.bottom(),
                "frame {frame}: paid-media bubble bottom ({}) exceeded list_rect.bottom() ({})",
                bubble.bottom(),
                list_rect.bottom()
            );
            assert!(
                bubble.top() >= list_rect.top(),
                "frame {frame}: paid-media bubble top ({}) was above list_rect.top() ({})",
                bubble.top(),
                list_rect.top()
            );
        }

        // Verify the paid-media unlock button is inside the viewport and clickable.
        harness.get_by_label("Unlock for 50 Stars").click();
        harness.step();
        assert!(
            harness.state().events.iter().any(|ev| matches!(
                ev,
                InnerEvent::UnlockPaidMedia {
                    msg_id,
                    stars_amount: 50,
                    ..
                } if *msg_id == paid_id
            )),
            "clicking paid-media unlock button at bottom of history must emit InnerEvent::UnlockPaidMedia"
        );

        // Appending a new paid-media bubble while already scrolled to bottom must keep it inside list_rect.
        let appended_id = MsgId(903);
        let appended = make_link_preview_message(
            peer,
            appended_id,
            BASE_UNIX + 90_020,
            "Bonus drop",
            LinkPreviewData {
                url: format!("paid_media:{}:903:75", peer.0),
                site_name: "Paid Media".to_owned(),
                title: "Unlock for 75 ⭐".to_owned(),
                description: "2 paid media item(s)".to_owned(),
                thumb_rgb: None,
                large_media: false,
                above_text: false,
            },
        );
        harness
            .state_mut()
            .inner
            .append_message(appended, PeerKind::User);
        for frame in 1..=10 {
            harness.step();
            let bubble = harness
                .state()
                .inner
                .last_painted_bubble_rect(appended_id)
                .unwrap_or_else(|| {
                    panic!("appended paid-media bubble must be painted on frame {frame}")
                });
            assert!(
                bubble.bottom() <= list_rect.bottom(),
                "after append frame {frame}: bubble bottom ({}) exceeded list_rect.bottom() ({})",
                bubble.bottom(),
                list_rect.bottom()
            );
        }
    }
}
