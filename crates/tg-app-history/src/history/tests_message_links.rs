//! TGB-2966: every link-like entity of a message body is a link span (drawn in the link colour,
//! clickable), as `Ui::Text::String` makes them in tdesktop (`text_block_parser.cpp`
//! `PrepareRichFromRich` keeps Url, CustomUrl, Mention, MentionName, Hashtag, Cashtag,
//! BotCommand, Email, Phone, BankCard and MediaTimestamp entities as links).
//!
//! `message_links_snapshot` writes `message_links.png` into `$MESSAGE_LINKS_SNAP_DIR` when that
//! variable is set. Mock data only.

use egui::{Pos2, Rect, Vec2};
use egui_kittest::HarnessBuilder;
use tg_text::{EntityInText, EntityType, TextWithEntities};
use tg_ui::entity_link::EntityLink;
use tg_ui::fonts::register_tdesktop_fonts;
use tg_ui::text::TextEntityKind;

use crate::history::backend::{HistoryMessage, HistoryPeerInfo, HistorySlice, MessageExtra};
use crate::history::inner::{HistoryInnerWidget, InnerEvent};
use crate::history::mock::MockHistoryStore;
use crate::history::text_bridge::entities_to_rich_text;
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind, PeerSummary};

const BASE_UNIX: i32 = 1_795_340_000;

fn utf16_len(s: &str) -> i32 {
    s.encode_utf16().count() as i32
}

/// Builds `text` from `(piece, entity)` parts; each entity covers its piece.
fn text_of(parts: &[(&str, Option<(EntityType, &str)>)]) -> TextWithEntities {
    let mut text = String::new();
    let mut entities = Vec::new();
    for (piece, entity) in parts {
        let offset = utf16_len(&text);
        text.push_str(piece);
        if let Some((ty, data)) = entity {
            entities.push(EntityInText::with_data(
                *ty,
                offset,
                utf16_len(piece),
                (*data).to_owned(),
            ));
        }
    }
    TextWithEntities { text, entities }
}

fn links_of(text: &TextWithEntities) -> Vec<(String, String)> {
    let rich = entities_to_rich_text(text);
    rich.spans
        .iter()
        .filter_map(|s| match &s.kind {
            TextEntityKind::Link(target) => {
                Some((rich.text[s.start..s.end].to_owned(), target.clone()))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn every_link_entity_of_a_message_body_is_a_link_span() {
    let text = text_of(&[
        (
            "https://t.me/c/1234567890/261238",
            Some((EntityType::Url, "")),
        ),
        (" ", None),
        (
            "site",
            Some((EntityType::CustomUrl, "https://example.org/a")),
        ),
        (" ", None),
        ("@crew_bot", Some((EntityType::Mention, ""))),
        (" ", None),
        ("Lia Moss", Some((EntityType::MentionName, "4242"))),
        (" ", None),
        ("#release", Some((EntityType::Hashtag, ""))),
        (" ", None),
        ("$TON", Some((EntityType::Cashtag, ""))),
        (" ", None),
        ("/start", Some((EntityType::BotCommand, ""))),
        (" ", None),
        ("crew@example.org", Some((EntityType::Email, ""))),
        (" ", None),
        ("0:12", Some((EntityType::MediaTimestamp, ""))),
    ]);
    let links = links_of(&text);
    let labels: Vec<&str> = links.iter().map(|(label, _)| label.as_str()).collect();
    assert_eq!(
        labels,
        vec![
            "https://t.me/c/1234567890/261238",
            "site",
            "@crew_bot",
            "Lia Moss",
            "#release",
            "$TON",
            "/start",
            "crew@example.org",
            "0:12",
        ]
    );
    assert_eq!(links[0].1, "https://t.me/c/1234567890/261238");
    assert_eq!(links[1].1, "https://example.org/a");
    // TGB-2973: hashtags, cashtags, bot commands and name mentions carry the target the block
    // renderer gives them too (`tg_ui::entity_link`).
    assert_eq!(links[3].1, EntityLink::MentionName(4242).target());
    assert_eq!(links[4].1, EntityLink::Hashtag("#release".into()).target());
    assert_eq!(links[5].1, EntityLink::Cashtag("$TON".into()).target());
    assert_eq!(links[6].1, EntityLink::BotCommand("/start".into()).target());
}

#[test]
fn text_without_entities_stays_plain_like_tdesktop() {
    // tdesktop draws a received message with `setMarkedText`: it only keeps the server's
    // entities and never detects links itself (`PrepareRichFromRich`).
    let text = TextWithEntities::simple("see https://example.org now");
    assert!(links_of(&text).is_empty());
}

fn peer() -> PeerId {
    PeerId::from_channel(77_966)
}

fn group_info(peer: PeerId) -> HistoryPeerInfo {
    HistoryPeerInfo {
        summary: PeerSummary {
            id: peer,
            kind: PeerKind::Group,
            title: "Studio Crew".to_owned(),
            status: "4 members".to_owned(),
            username: None,
            phone_or_link: None,
            about: String::new(),
            initials: "SC".to_owned(),
            avatar_rgb: [0x5E, 0x72, 0xE4],
            muted: false,
            can_send: true,
            is_saved_messages: false,
        },
        typing_text: None,
        contact_status: None,
        unread_count: 0,
        can_pin: false,
        can_delete: false,
        link_base: String::new(),
    }
}

fn message(id: i32, from: u64, name: &str, text: TextWithEntities) -> HistoryMessage {
    HistoryMessage {
        id: MsgId(id),
        peer_id: peer(),
        from_id: PeerId::from_user(from),
        sender_name: name.to_owned(),
        date_unix: BASE_UNIX + id * 60,
        outgoing: false,
        is_post: false,
        text,
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

fn snapshot_messages() -> Vec<HistoryMessage> {
    vec![
        message(
            1,
            1004,
            "Tavi Roan",
            text_of(&[(
                "https://t.me/c/1234567890/261238",
                Some((EntityType::Url, "")),
            )]),
        ),
        message(
            2,
            1002,
            "Ivo Marten",
            text_of(&[
                ("Ask ", None),
                ("Lia Moss", Some((EntityType::MentionName, "4242"))),
                (" or ", None),
                ("@crew_bot", Some((EntityType::Mention, ""))),
                (" about ", None),
                ("#release", Some((EntityType::Hashtag, ""))),
                (" and ", None),
                ("$TON", Some((EntityType::Cashtag, ""))),
                (", then ", None),
                ("/start", Some((EntityType::BotCommand, ""))),
            ]),
        ),
        message(
            3,
            1003,
            "Nell Ashby",
            text_of(&[
                ("Notes are on the ", None),
                (
                    "team site",
                    Some((EntityType::CustomUrl, "https://example.org/notes")),
                ),
                (", questions to ", None),
                ("crew@example.org", Some((EntityType::Email, ""))),
            ]),
        ),
    ]
}

struct SnapState {
    inner: HistoryInnerWidget,
    store: MockHistoryStore,
    peer_info: HistoryPeerInfo,
    list_rect: Rect,
    colors: HistoryColors,
    events: Vec<InnerEvent>,
}

#[test]
fn message_links_snapshot() {
    let size = Vec2::new(520.0, 260.0);
    let messages = snapshot_messages();
    let mut inner = HistoryInnerWidget::new();
    let total_count = messages.len();
    inner.apply_slice(
        peer(),
        HistorySlice {
            messages,
            loaded_at_top: true,
            loaded_at_bottom: true,
            unread_count: 0,
            first_unread_id: None,
            total_count,
        },
        PeerKind::Group,
        true,
    );
    let state = SnapState {
        inner,
        store: MockHistoryStore::new(),
        peer_info: group_info(peer()),
        list_rect: Rect::from_min_size(Pos2::ZERO, size),
        colors: HistoryColors::from_palette(&Palette::for_mode(ThemeMode::Day)),
        events: Vec::new(),
    };
    let mut harness = HarnessBuilder::default()
        .with_size(size)
        .with_step_dt(0.05)
        .with_max_steps(8)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .wgpu()
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
        })
        .build_ui_state(
            |ui, st: &mut SnapState| {
                ui.painter()
                    .rect_filled(st.list_rect, 0.0, st.colors.canvas_bg);
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
    harness.run_steps(4);
    if let Some(dir) = std::env::var_os("MESSAGE_LINKS_SNAP_DIR") {
        let img = harness.render().expect("render message links snapshot");
        let path = std::path::PathBuf::from(dir).join("message_links.png");
        img.save(&path).expect("save message links snapshot");
    }
}
