//! Screenshot harness for group sender lines and channel reactions (TGB-2953, mock data).
//!
//! Writes `group_<theme>.png` and `channel_<theme>.png` into `$BUBBLE_LAYOUT_SNAP_DIR` when that
//! variable is set; otherwise it only renders the frames (a smoke test for the painter).

use egui::{Pos2, Rect, Vec2};
use egui_kittest::HarnessBuilder;
use tg_text::TextWithEntities;
use tg_ui::fonts::register_tdesktop_fonts;

use crate::history::backend::{
    HistoryMessage, HistoryPeerInfo, HistorySlice, MessageExtra, ReactionCounter,
};
use crate::history::inner::HistoryInnerWidget;
use crate::history::mock::MockHistoryStore;
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind, PeerSummary};

const BASE_UNIX: i32 = 1_795_340_000;

struct SnapState {
    inner: HistoryInnerWidget,
    store: MockHistoryStore,
    peer_info: HistoryPeerInfo,
    list_rect: Rect,
    colors: HistoryColors,
}

fn peer_info(peer: PeerId, kind: PeerKind, title: &str) -> HistoryPeerInfo {
    HistoryPeerInfo {
        summary: PeerSummary {
            id: peer,
            kind,
            title: title.to_owned(),
            status: String::new(),
            username: None,
            phone_or_link: None,
            about: String::new(),
            initials: "TG".to_owned(),
            avatar_rgb: [0x6C, 0x9F, 0xE0],
            muted: false,
            can_send: kind == PeerKind::Group,
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

fn message(peer: PeerId, id: i32, from: PeerId, name: &str, text: &str) -> HistoryMessage {
    HistoryMessage {
        id: MsgId(id),
        peer_id: peer,
        from_id: from,
        sender_name: name.to_owned(),
        date_unix: BASE_UNIX + id * 20,
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
        extra: MessageExtra::default(),
    }
}

// SNAP-DECORATE-BEGIN
fn decorate(msgs: &mut [HistoryMessage]) {
    use crate::history::backend::{SenderBadge, SenderBadgeRole, SenderNameStatus};
    for m in msgs {
        let (badge, premium) = match m.from_id.bare() {
            501 => (Some(("котик", SenderBadgeRole::User)), true),
            502 => (Some(("Главный Модератор", SenderBadgeRole::Admin)), true),
            503 => (Some(("owner", SenderBadgeRole::Creator)), false),
            _ => (None, false),
        };
        m.extra.sender_badge = badge.map(|(text, role)| SenderBadge {
            text: text.to_owned(),
            role,
        });
        m.extra.sender_status = premium.then_some(SenderNameStatus {
            emoji_status_id: None,
            premium: true,
        });
    }
}
// SNAP-DECORATE-END

fn group_messages() -> (PeerId, Vec<HistoryMessage>) {
    let peer = PeerId::from_chat(88_001);
    let a = PeerId(501);
    let b = PeerId(502);
    let c = PeerId(503);
    let mut msgs = vec![
        message(peer, 1, a, "Тестовая Оля", "кто уже пробовал новую сборку?"),
        message(peer, 2, a, "Тестовая Оля", "у меня пока всё работает"),
        message(peer, 3, a, "Тестовая Оля", "ок"),
        message(
            peer,
            4,
            b,
            "Sample Moderator",
            "а у меня список чатов прокручивается без рывков",
        ),
        message(peer, 5, b, "Sample Moderator", "проверю вечером"),
        message(peer, 6, c, "Demo Owner", "спасибо всем за тесты"),
    ];
    decorate(&mut msgs);
    (peer, msgs)
}

fn channel_messages() -> (PeerId, Vec<HistoryMessage>) {
    let peer = PeerId::from_channel(88_002);
    let mut post = message(
        peer,
        1,
        peer,
        "Studio Notes",
        "We made a mistake in yesterday's build notes: the release moved to Friday.\n\n\
         The fixed notes are online now. Thanks to everyone who reported it so quickly, \
         and sorry for the noise in your feeds this week.",
    );
    post.is_post = true;
    post.views = Some(14_512);
    post.edited = true;
    post.reactions = [
        ("💩", 301),
        ("🤣", 59),
        ("🔥", 23),
        ("😡", 15),
        ("❤️", 11),
        ("😭", 11),
        ("👍", 8),
        ("🤡", 7),
        ("🙏", 7),
        ("👎", 1),
        ("😁", 1),
    ]
    .iter()
    .map(|(e, c)| ReactionCounter {
        emoji: (*e).to_owned(),
        count: *c,
        chosen: false,
    })
    .collect();
    let mut short = message(peer, 2, peer, "Studio Notes", "Fixed notes are online.");
    short.is_post = true;
    short.views = Some(8_841);
    short.reactions = [("❤️", 85), ("💩", 53), ("👍", 22)]
        .iter()
        .map(|(e, c)| ReactionCounter {
            emoji: (*e).to_owned(),
            count: *c,
            chosen: false,
        })
        .collect();
    (peer, vec![post, short])
}

fn render(
    peer: PeerId,
    kind: PeerKind,
    messages: Vec<HistoryMessage>,
    mode: ThemeMode,
    size: Vec2,
) -> Option<image::RgbaImage> {
    let mut inner = HistoryInnerWidget::new();
    let total_count = messages.len();
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
        kind,
        true,
    );
    let state = SnapState {
        inner,
        store: MockHistoryStore::new(),
        peer_info: peer_info(peer, kind, "Snapshot"),
        list_rect: Rect::from_min_size(Pos2::ZERO, size),
        colors: HistoryColors::from_palette(&Palette::for_mode(mode)),
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
    std::env::var_os("BUBBLE_LAYOUT_SNAP_DIR").map(|_| harness.render().expect("render snapshot"))
}

#[test]
fn group_names_and_channel_reactions_snapshot() {
    let dir = std::env::var_os("BUBBLE_LAYOUT_SNAP_DIR").map(std::path::PathBuf::from);
    for (tag, mode) in [("day", ThemeMode::Day), ("night", ThemeMode::Night)] {
        let (peer, msgs) = group_messages();
        let group = render(peer, PeerKind::Group, msgs, mode, Vec2::new(620.0, 460.0));
        let (peer, msgs) = channel_messages();
        let channel = render(peer, PeerKind::Channel, msgs, mode, Vec2::new(620.0, 560.0));
        if let Some(ref dir) = dir {
            std::fs::create_dir_all(dir).expect("create snap dir");
            for (name, img) in [("group", group), ("channel", channel)] {
                if let Some(img) = img {
                    img.save(dir.join(format!("{name}_{tag}.png")))
                        .expect("save snapshot");
                }
            }
        }
    }
}
