//! TGB-3017 evidence and regression: channel posts with paid, custom emoji and regular reactions.
//!
//! `reactions_row_snapshot` writes `reactions_row.png` into `$REACTIONS_SNAP_DIR` when that
//! variable is set. Mock data only; the custom emoji is a synthetic heart bitmap.
//! The test also checks that no reaction button paints outside its own button: before
//! TGB-3017 a `custom:<document id>` key was drawn as text centred on the 18 px image square and
//! spilled over the neighbouring buttons and past the bubble.

use egui::{Pos2, Rect, Vec2};
use egui_kittest::HarnessBuilder;
use tg_text::TextWithEntities;
use tg_ui::custom_emoji::{CustomEmojiAsset, register_custom_emoji_asset};
use tg_ui::fonts::register_tdesktop_fonts;

use crate::history::backend::{
    HistoryMessage, HistoryPeerInfo, HistorySlice, MessageExtra, ReactionCounter,
};
use crate::history::inner::{HistoryInnerWidget, InnerEvent};
use crate::history::mock::MockHistoryStore;
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind, PeerSummary};

const BASE_UNIX: i32 = 1_795_340_000;
/// Columns left of every bubble.
const LEFT_GUTTER: u32 = 14;
/// A custom emoji id as long as the real ones (19 digits).
const LOADED_ID: u64 = 5_199_576_980_080_601_299;
/// A custom emoji whose document has not arrived yet.
const PENDING_ID: u64 = 5_170_512_345_678_901_234;

fn peer() -> PeerId {
    PeerId::from_channel(73_017)
}

fn heart_asset() -> CustomEmojiAsset {
    let side = 64u32;
    let mut rgba = Vec::with_capacity((side * side * 4) as usize);
    for y in 0..side {
        for x in 0..side {
            let fx = (x as f32 / side as f32 - 0.5) * 2.6;
            let fy = (0.5 - y as f32 / side as f32) * 2.6 + 0.2;
            let v = (fx * fx + fy * fy - 1.0).powi(3) - fx * fx * fy.powi(3);
            if v <= 0.0 {
                rgba.extend_from_slice(&[0xE5, 0x3A, 0x5C, 0xFF]);
            } else {
                rgba.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    CustomEmojiAsset::static_rgba(side, side, rgba)
}

fn channel_info() -> HistoryPeerInfo {
    HistoryPeerInfo {
        summary: PeerSummary {
            id: peer(),
            kind: PeerKind::Channel,
            title: "Night Shift Notes".to_owned(),
            status: "12,400 subscribers".to_owned(),
            username: None,
            phone_or_link: None,
            about: String::new(),
            initials: "NS".to_owned(),
            avatar_rgb: [0x5E, 0x72, 0xE4],
            muted: false,
            can_send: false,
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

fn reaction(emoji: &str, count: u32, chosen: bool) -> ReactionCounter {
    ReactionCounter {
        emoji: emoji.to_owned(),
        count,
        chosen,
    }
}

fn post(id: i32, text: &str, reactions: Vec<ReactionCounter>) -> HistoryMessage {
    HistoryMessage {
        id: MsgId(id),
        peer_id: peer(),
        from_id: peer(),
        sender_name: "Night Shift Notes".to_owned(),
        date_unix: BASE_UNIX + id * 60,
        outgoing: false,
        is_post: true,
        text: TextWithEntities::simple(text),
        media: None,
        service: None,
        reply: None,
        forward: None,
        reactions,
        views: Some(12_900),
        edited: false,
        is_read: true,
        is_sending: false,
        is_pinned: false,
        extra: MessageExtra {
            replies_count: Some(33),
            ..MessageExtra::default()
        },
    }
}

fn posts() -> Vec<HistoryMessage> {
    vec![
        post(
            1,
            "1:41:54 Stream recap\n1:45:08 Questions\n1:49:56 The end",
            vec![
                reaction("⭐", 12, false),
                reaction("👍", 2_300, false),
                reaction(&format!("custom:{LOADED_ID}"), 1_400, false),
                reaction("🔥", 640, false),
                reaction(&format!("custom:{PENDING_ID}"), 95, false),
                reaction("😢", 9, true),
            ],
        ),
        post(
            2,
            "Short post with a few reactions",
            vec![
                reaction(&format!("custom:{LOADED_ID}"), 7, true),
                reaction("⭐", 3, true),
            ],
        ),
        post(
            3,
            "Its custom emoji is still loading",
            vec![
                reaction(&format!("custom:{PENDING_ID}"), 95, false),
                reaction("👍", 4, false),
            ],
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

fn render(messages: Vec<HistoryMessage>) -> image::RgbaImage {
    let size = Vec2::new(520.0, 440.0);
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
        PeerKind::Channel,
        true,
    );
    let state = SnapState {
        inner,
        store: MockHistoryStore::new(),
        peer_info: channel_info(),
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
    harness.render().expect("render reactions row snapshot")
}

#[test]
fn reactions_row_snapshot() {
    assert!(register_custom_emoji_asset(
        LOADED_ID.to_string(),
        &heart_asset()
    ));
    let img = render(posts());
    if let Some(dir) = std::env::var_os("REACTIONS_SNAP_DIR") {
        let path = std::path::PathBuf::from(dir).join("reactions_row.png");
        img.save(&path).expect("save reactions row snapshot");
    }

    // No reaction key is ever painted as text: before TGB-3017 the 26-character
    // `custom:<document id>` key was drawn centred on its 18 px image square, about 200 px wide,
    // so the first button of post 3 spilled text far left of its bubble. The columns left of the
    // bubbles must look exactly as in the same posts with plain emoji reactions (the button
    // widths depend only on the counts).
    let plain: Vec<HistoryMessage> = posts()
        .into_iter()
        .map(|mut m| {
            for r in &mut m.reactions {
                r.emoji = "👍".to_owned();
            }
            m
        })
        .collect();
    let reference = render(plain);
    let mut stray = 0usize;
    for (x, y, px) in img.enumerate_pixels() {
        if x < LEFT_GUTTER && px != reference.get_pixel(x, y) {
            stray += 1;
        }
    }
    assert_eq!(stray, 0, "reaction text spilled left of the bubbles");
}
