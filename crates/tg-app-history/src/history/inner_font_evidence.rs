//! Manual evidence render for the font / bubble-layout parity work (not a golden test).
//!
//! `TGF_EVIDENCE_OUT=<png> cargo test -p tg-app-history font_evidence -- --ignored --nocapture`
//! renders mock bubbles with Latin (the strings of the official 7.4.6 capture
//! `uiparity/02_one_to_one_chat`), Russian, Ukrainian and code text, a photo caption and a
//! hovered short outgoing bubble in a wide chat. `TGF_SYSTEM_MONO=1` turns on the system
//! monospace face like the app binary does.

use egui::{Pos2, Rect, Vec2};
use egui_kittest::HarnessBuilder;
use tg_text::{EntityInText, EntityType, TextWithEntities};

use super::*;
use crate::history::backend::{HistoryPeerInfo, HistorySlice, MessageExtra, PhotoAttachment};
use crate::history::mock::MockHistoryStore;
use tg_app_core::stand_in::types::PeerSummary;

const BASE_UNIX: i32 = 1_795_340_000;

fn utf16_len(s: &str) -> i32 {
    s.encode_utf16().count() as i32
}

fn rich(text: &str, marks: &[(EntityType, &str)]) -> TextWithEntities {
    let mut out = TextWithEntities::simple(text);
    for (kind, part) in marks {
        if let Some(byte) = text.find(part) {
            let offset = utf16_len(&text[..byte]);
            out.entities
                .push(EntityInText::new(*kind, offset, utf16_len(part)));
        }
    }
    out
}

struct Spec {
    id: i32,
    outgoing: bool,
    text: TextWithEntities,
    photo: Option<(i32, i32)>,
}

fn msg(peer: PeerId, n: usize, s: Spec) -> HistoryMessage {
    HistoryMessage {
        id: MsgId(s.id),
        peer_id: peer,
        from_id: if s.outgoing {
            PeerId::from_user(1)
        } else {
            peer
        },
        sender_name: "Test2name Test2surname".to_owned(),
        date_unix: BASE_UNIX + n as i32 * 60,
        outgoing: s.outgoing,
        is_post: false,
        text: s.text,
        media: s.photo.map(|(width, height)| {
            HistoryMedia::Photo(PhotoAttachment {
                width,
                height,
                stripped_thumb: None,
                fallback_rgb: ([0x52, 0x1C, 0x1C], [0x22, 0x0A, 0x0A]),
                file_name: "frame.jpg".to_owned(),
                size_bytes: 10_000,
            })
        }),
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

fn specs() -> Vec<Spec> {
    let premium = "Новая покупка Premium!\n\nID: 5213814913\nТариф: day (1 дн.)\nСумма: 85 XTR\n\
                   Транзакция: 8a77b8bb4d10\nПремиум до: 23.11.2026 03:10 UTC";
    let ukrainian = "Ґанок, їжак, єнот і ґудзик. Місцеві заклади вже почали працювати: \
                     «Їжа є!» — код let ї = 1;";
    vec![
        Spec {
            id: 1,
            outgoing: true,
            text: TextWithEntities::simple(
                "Hello! Checking 1:1 chat parity with text, reply, photo, file, and reactions.",
            ),
            photo: None,
        },
        Spec {
            id: 2,
            outgoing: false,
            text: TextWithEntities::simple(
                "Hi from Account B! Testing incoming message bubble and reply styling.",
            ),
            photo: None,
        },
        Spec {
            id: 3,
            outgoing: false,
            text: rich(
                premium,
                &[
                    (EntityType::Bold, "Новая покупка Premium!"),
                    (EntityType::Code, "5213814913"),
                    (EntityType::Code, "8a77b8bb4d10"),
                ],
            ),
            photo: None,
        },
        Spec {
            id: 4,
            outgoing: false,
            text: rich(ukrainian, &[(EntityType::Code, "let ї = 1;")]),
            photo: None,
        },
        Spec {
            id: 5,
            outgoing: false,
            text: TextWithEntities::simple(
                "у моей девушки прохождение клаттерфанка в геометри дэш в 360р на ютубе \
                 собрало 34к просмотров",
            ),
            photo: Some((420, 520)),
        },
        Spec {
            id: 6,
            outgoing: true,
            text: TextWithEntities::simple("Так"),
            photo: None,
        },
    ]
}

struct State {
    inner: HistoryInnerWidget,
    store: MockHistoryStore,
    peer_info: HistoryPeerInfo,
    colors: HistoryColors,
}

#[test]
#[ignore = "writes an evidence PNG; run by hand with TGF_EVIDENCE_OUT"]
fn font_evidence() {
    let Some(out) = std::env::var_os("TGF_EVIDENCE_OUT") else {
        return;
    };
    if std::env::var_os("TGF_SYSTEM_MONO").is_some() {
        tg_ui::fonts::enable_system_monospace();
    }
    let peer = PeerId::from_user(91_077);
    let messages: Vec<_> = specs()
        .into_iter()
        .enumerate()
        .map(|(n, s)| msg(peer, n, s))
        .collect();
    let total_count = messages.len();
    let mut inner = HistoryInnerWidget::new();
    inner.apply_slice(
        peer,
        HistorySlice {
            messages,
            loaded_at_top: true,
            loaded_at_bottom: true,
            unread_count: 0,
            first_unread_id: None,
            total_count,
        },
        PeerKind::User,
        true,
    );
    let state = State {
        inner,
        store: MockHistoryStore::new(),
        peer_info: HistoryPeerInfo {
            summary: PeerSummary {
                id: peer,
                kind: PeerKind::User,
                title: "Test2name Test2surname".to_owned(),
                status: "online".to_owned(),
                username: None,
                phone_or_link: None,
                about: String::new(),
                initials: "TT".to_owned(),
                avatar_rgb: [0x8B, 0x6C, 0xE6],
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
        },
        colors: HistoryColors::from_palette(&Palette::for_mode(ThemeMode::Day)),
    };
    let list_rect = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(726.0, 1040.0));
    let mut harness = HarnessBuilder::default()
        .with_size(Vec2::new(726.0, 1040.0))
        .with_step_dt(0.05)
        .with_max_steps(8)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(tg_ui::fonts::register_tdesktop_fonts)
        .build_ui_state(
            |ui, st: &mut State| {
                // Wide chat: every bubble is left-aligned, like the owner's screenshot.
                let _ = st.inner.show(
                    ui,
                    list_rect,
                    &st.peer_info,
                    true,
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
    if let Some(bubble) = harness.state().inner.last_painted_bubble_rect(MsgId(6)) {
        harness.hover_at(bubble.center());
        for _ in 0..8 {
            harness.step();
        }
        println!("measured: short outgoing bubble {bubble:?}");
    }
    for id in 1..=6 {
        if let Some(r) = harness.state().inner.last_painted_bubble_rect(MsgId(id)) {
            println!(
                "measured: bubble {id}: {:.0}x{:.0} at {:?}",
                r.width(),
                r.height(),
                r.min
            );
        }
    }
    let image = harness.render().expect("wgpu render");
    image.save(&out).expect("save evidence png");
    println!("wrote {}", std::path::Path::new(&out).display());
}
