//! TGB-3012: sender name colours in groups match tdesktop's `Ui::FromNameFg`.
//!
//! Expected values come from tdesktop's formulas: `ColorIndexToPaletteIndex`
//! (`{ 0, 7, 4, 1, 6, 3, 5 }`), `historyPeerNNameFg` of `colors.palette` (day) and
//! `night.tdesktop-theme` (night), the server colour sets for indexes past 6, and collectible
//! accents.
//!
//! `name_colors_snapshot` writes `names_day.png` and `names_night.png` into
//! `$TGB3012_SNAP_DIR` when set. With `$TGB3012_SNAP_BEFORE` set it reproduces the old code
//! paths instead (names by `id % 7`, day name colours in the night theme). Mock data only.

use egui::{Color32, Pos2, Rect, Vec2};
use egui_kittest::HarnessBuilder;
use tg_text::TextWithEntities;
use tg_ui::fonts::register_tdesktop_fonts;
use tg_ui::peer_color::{
    CollectibleAccent, ColorIndexData, ColorIndices, PeerAccent, set_color_indices,
};
use tg_ui::theme::Colors;

use crate::history::backend::{
    HistoryMessage, HistoryPeerInfo, HistorySlice, MessageExtra, ReplyHeader,
};
use crate::history::inner::{HistoryInnerWidget, InnerEvent};
use crate::history::mock::MockHistoryStore;
use crate::history::name_line::{reply_author_name_fg, sender_name_fg};
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind, PeerSummary};

const BASE_UNIX: i32 = 1_795_340_000;
/// `777000 % 7 == 0`: red by id.
const RED_BY_ID: u64 = 777_000;

const DAY_RED: Color32 = Color32::from_rgb(0xc0, 0x3d, 0x33);
const DAY_BLUE: Color32 = Color32::from_rgb(0x16, 0x8a, 0xcd);
const NIGHT_RED: Color32 = Color32::from_rgb(0xfb, 0x61, 0x69);
const NIGHT_BLUE: Color32 = Color32::from_rgb(0x65, 0xbd, 0xf3);

fn peer() -> PeerId {
    PeerId::from_channel(73_012)
}

fn message(
    id: i32,
    from: u64,
    name: &str,
    text: &str,
    accent: Option<PeerAccent>,
) -> HistoryMessage {
    HistoryMessage {
        id: MsgId(id),
        peer_id: peer(),
        from_id: PeerId::from_user(from),
        sender_name: name.to_owned(),
        date_unix: BASE_UNIX + id * 60,
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
            sender_accent: accent,
            ..MessageExtra::default()
        },
    }
}

fn accent(index: u8) -> PeerAccent {
    PeerAccent {
        index,
        collectible: None,
    }
}

fn day() -> HistoryColors {
    HistoryColors::from_palette(&Palette::for_mode(ThemeMode::Day))
}

fn night() -> HistoryColors {
    HistoryColors::from_palette(&Palette::for_mode(ThemeMode::Night))
}

#[test]
fn night_theme_resolves_its_own_name_colours() {
    // The old adapter fell back to the day palette for every name it did not map, so night
    // names were the day ones (`#c03d33` instead of `#fb6169`).
    let ui = night().ui_colors();
    assert_eq!(ui.color("historyPeer1NameFg"), NIGHT_RED);
    assert_eq!(ui.color("historyPeer4NameFg"), NIGHT_BLUE);
    assert_eq!(ui.color("historyPeer8NameFgSelected"), Color32::WHITE);
    let ui = day().ui_colors();
    assert_eq!(ui.color("historyPeer1NameFg"), DAY_RED);
    assert_eq!(ui.color("historyPeer1NameFgSelected"), DAY_RED);
}

#[test]
fn sender_name_takes_the_server_colour_index() {
    let colors = day();
    let ui = colors.ui_colors();
    // Red by id, but the server colour is 5 (palette slot 3, blue).
    let msg = message(1, RED_BY_ID, "Rin", "hi", Some(accent(5)));
    assert_eq!(sender_name_fg(&msg, &colors, &ui, false), DAY_BLUE);
    // Without a server colour (mock data) the id decides.
    let msg = message(1, RED_BY_ID, "Rin", "hi", None);
    assert_eq!(sender_name_fg(&msg, &colors, &ui, false), DAY_RED);

    let colors = night();
    let ui = colors.ui_colors();
    let msg = message(1, RED_BY_ID, "Rin", "hi", Some(accent(5)));
    assert_eq!(sender_name_fg(&msg, &colors, &ui, false), NIGHT_BLUE);
    assert_eq!(sender_name_fg(&msg, &colors, &ui, true), Color32::WHITE);
}

#[test]
fn collectible_sender_uses_its_accent_per_theme() {
    let mut msg = message(1, RED_BY_ID, "Rin", "hi", None);
    msg.extra.sender_accent = Some(PeerAccent {
        index: 0,
        collectible: Some(CollectibleAccent {
            day: 0xe0_86_1f,
            night: 0xff_b3_5c,
        }),
    });
    let colors = day();
    assert_eq!(
        sender_name_fg(&msg, &colors, &colors.ui_colors(), false),
        Color32::from_rgb(0xe0, 0x86, 0x1f)
    );
    let colors = night();
    assert_eq!(
        sender_name_fg(&msg, &colors, &colors.ui_colors(), false),
        Color32::from_rgb(0xff, 0xb3, 0x5c)
    );
}

#[test]
fn reply_author_name_takes_the_author_accent() {
    let header = ReplyHeader {
        reply_to_msg_id: MsgId(1),
        author_name: "Rin".to_owned(),
        author_color_id: RED_BY_ID,
        author_accent: Some(accent(5)),
        preview_text: "hi".to_owned(),
        preview_thumb_rgb: None,
        preview_thumb: None,
        preview_colored_len: 0,
        quote_text: None,
    };
    let colors = day();
    let ui = colors.ui_colors();
    assert_eq!(reply_author_name_fg(&header, &colors, &ui, false), DAY_BLUE);
    let without = ReplyHeader {
        author_accent: None,
        ..header
    };
    assert_eq!(reply_author_name_fg(&without, &colors, &ui, false), DAY_RED);
}

fn group_info() -> HistoryPeerInfo {
    HistoryPeerInfo {
        summary: PeerSummary {
            id: peer(),
            kind: PeerKind::Group,
            title: "Colour Lab".to_owned(),
            status: "5 members".to_owned(),
            username: None,
            phone_or_link: None,
            about: String::new(),
            initials: "CL".to_owned(),
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

struct SnapState {
    inner: HistoryInnerWidget,
    store: MockHistoryStore,
    peer_info: HistoryPeerInfo,
    list_rect: Rect,
    colors: HistoryColors,
    events: Vec<InnerEvent>,
}

/// Senders whose tdesktop colour differs from `id % 7` in every way the server can set it.
fn snapshot_messages(before: bool) -> Vec<HistoryMessage> {
    let pick = |a: PeerAccent| (!before).then_some(a);
    vec![
        // id % 7 = 0 (red); server colour 5 -> blue.
        message(
            1,
            RED_BY_ID,
            "Server index 5",
            "blue in tdesktop",
            pick(accent(5)),
        ),
        // 1101 % 7 = 2 (purple); no server colour -> purple in both.
        message(
            2,
            1101,
            "No server colour",
            "purple: id % 7 = 2",
            pick(accent(2)),
        ),
        // 1102 % 7 = 3 (green); server colour 10 -> the server's two-colour set.
        message(
            3,
            1102,
            "Server index 10",
            "first colour of its set",
            pick(accent(10)),
        ),
        // 1103 % 7 = 4 (sea); collectible colour -> its accent.
        message(
            4,
            1103,
            "Collectible colour",
            "the gift's accent",
            pick(PeerAccent {
                index: 4,
                collectible: Some(CollectibleAccent {
                    day: 0xe0_86_1f,
                    night: 0xff_b3_5c,
                }),
            }),
        ),
        // 1104 % 7 = 5 (blue); server colour 1 -> orange.
        message(
            5,
            1104,
            "Server index 1",
            "orange in tdesktop",
            pick(accent(1)),
        ),
    ]
}

fn render(colors: HistoryColors, before: bool, file: &str) {
    let size = Vec2::new(420.0, 330.0);
    let messages = snapshot_messages(before);
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
        peer_info: group_info(),
        list_rect: Rect::from_min_size(Pos2::ZERO, size),
        colors,
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
    if let Some(dir) = std::env::var_os("TGB3012_SNAP_DIR") {
        let img = harness.render().expect("render name colour snapshot");
        img.save(std::path::PathBuf::from(dir).join(file))
            .expect("save name colour snapshot");
    }
}

#[test]
fn name_colors_snapshot() {
    if std::env::var_os("TGB3012_SNAP_DIR").is_none() {
        return;
    }
    let before = std::env::var_os("TGB3012_SNAP_BEFORE").is_some();
    // A two-colour set for index 10 as `help.peerColors` sends it (blue / red).
    let mut table: ColorIndices = [ColorIndexData::default(); 64];
    table[10] = ColorIndexData {
        light: [Some(0x3e_88_f7), Some(0xff_32_a0), None],
        dark: [Some(0x52_bf_ff), Some(0xff_59_7b), None],
    };
    set_color_indices(table);
    render(day(), before, "names_day.png");
    let mut night = night();
    if before {
        // The old adapter resolved `historyPeerNNameFg` from the day palette.
        night.peer_name_fg = Palette::day().peer_name_fg;
    }
    render(night, before, "names_night.png");
}
