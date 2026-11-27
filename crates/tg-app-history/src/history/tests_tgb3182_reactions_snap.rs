//! TGB-3182: inline reactions like tdesktop's `Reactions::InlineList` (night theme, a group).
//!
//! - A photo without a caption: the reactions go below the bubble on the chat background
//!   (`embedReactionsInBubble` is false for media drawing its own time), a chosen button is the
//!   light `msgServiceFg` pill, and a reaction whose reactors the session knows shows their
//!   userpics instead of the count (`InlineListDataFromMessage`, at most three reactions).
//! - A text message keeps its reactions inside the bubble, with userpics when known.
//! - Many reactions keep their counts.
//!
//! `$TGB3182_SNAP_DIR` set: the chat is saved there as `reactions_after.png`. Mock data only; the
//! reactors' userpics are a synthetic photo.
#![cfg(windows)]

use std::sync::Arc;

use egui::{Pos2, Rect, Vec2};
use egui_kittest::HarnessBuilder;
use tg_text::TextWithEntities;
use tg_ui::fonts::register_tdesktop_fonts;
use tg_ui::userpic_images::{UserpicImage, UserpicImageSource, install_userpic_source};

use crate::history::backend::{
    HistoryMedia, HistoryMessage, HistorySlice, MessageExtra, PhotoAttachment, ReactionCounter,
    RecentReactors,
};
use crate::history::inner::HistoryInnerWidget;
use crate::history::mock::MockHistoryStore;
use crate::history::tests_w4_text_snap::{InnerState, group_info, group_peer};
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind};

const BASE_UNIX: i32 = 1_795_340_000;
/// The synthetic userpic: solid orange, a colour nothing else uses.
const ORANGE: [u8; 3] = [0xF0, 0x80, 0x10];
const READER: u64 = 3_182_001;

struct OrangePhotos;

impl UserpicImageSource for OrangePhotos {
    fn userpic(&self, _peer_id: i64) -> Option<Arc<UserpicImage>> {
        let side = 40;
        let mut rgba = Vec::with_capacity(side * side * 4);
        for _ in 0..side * side {
            rgba.extend_from_slice(&[ORANGE[0], ORANGE[1], ORANGE[2], 0xFF]);
        }
        Some(Arc::new(UserpicImage {
            photo_id: 9,
            width: side,
            height: side,
            rgba,
        }))
    }
}

fn reaction(emoji: &str, count: u32, chosen: bool) -> ReactionCounter {
    ReactionCounter {
        emoji: emoji.to_owned(),
        count,
        chosen,
    }
}

fn base(id: i32, text: &str) -> HistoryMessage {
    HistoryMessage {
        id: MsgId(id),
        peer_id: group_peer(),
        from_id: PeerId::from_user(3_182_100),
        sender_name: "Mira Holt".to_owned(),
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
        extra: MessageExtra::default(),
    }
}

fn known(reaction: &str, peers: &[(u64, &str)]) -> RecentReactors {
    RecentReactors {
        reaction: reaction.to_owned(),
        peers: peers
            .iter()
            .map(|&(id, name)| (PeerId::from_user(id), name.to_owned()))
            .collect(),
    }
}

/// The owner's case: a photo, the user's own heart, the session knows who reacted.
pub(crate) fn photo_with_heart() -> HistoryMessage {
    let mut msg = base(1, "");
    msg.media = Some(HistoryMedia::Photo(PhotoAttachment {
        width: 640,
        height: 800,
        stripped_thumb: None,
        fallback_rgb: ([0x6A, 0x6A, 0x70], [0x3A, 0x3A, 0x40]),
        file_name: "photo.jpg".to_owned(),
        size_bytes: 120_000,
    }));
    msg.reactions = vec![reaction("❤", 1, true)];
    msg.extra.recent_reactors = vec![known("❤", &[(READER, "Self Reader")])];
    msg
}

fn messages() -> Vec<HistoryMessage> {
    let mut text = base(2, "Two people liked this one");
    text.reactions = vec![reaction("👍", 2, false)];
    text.extra.recent_reactors = vec![known(
        "👍",
        &[(3_182_002, "Ivo Marten"), (3_182_003, "Lena Park")],
    )];
    let mut busy = base(3, "Too many reactions for userpics");
    busy.reactions = vec![reaction("🔥", 14, true), reaction("👍", 3, false)];
    vec![photo_with_heart(), text, busy]
}

fn render(messages: Vec<HistoryMessage>) -> image::RgbaImage {
    let size = Vec2::new(560.0, 760.0);
    let peer = group_peer();
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
        PeerKind::Group,
        true,
    );
    let state = InnerState {
        inner,
        store: MockHistoryStore::new(),
        peer_info: group_info(peer),
        list_rect: Rect::from_min_size(Pos2::ZERO, size),
        colors: HistoryColors::from_palette(&Palette::for_mode(ThemeMode::Night)),
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
            install_userpic_source(ctx, Arc::new(OrangePhotos));
        })
        .build_ui_state(
            |ui, st: &mut InnerState| {
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
    harness.render().expect("render reactions")
}

fn near(px: &image::Rgba<u8>, rgb: [u8; 3], tol: i32) -> bool {
    px.0[..3]
        .iter()
        .zip(rgb)
        .all(|(&a, b)| (i32::from(a) - i32::from(b)).abs() <= tol)
}

#[test]
fn reactions_show_userpics_and_sit_below_a_photo() {
    let img = render(messages());
    if let Some(dir) = std::env::var_os("TGB3182_SNAP_DIR") {
        img.save(std::path::PathBuf::from(dir).join("reactions_after.png"))
            .expect("save snapshot");
    }
    let colors = HistoryColors::from_palette(&Palette::for_mode(ThemeMode::Night));
    let bubble_bg = colors.msg_in_bg;
    let pill = colors.service_fg;

    // The reactors' userpics are drawn (photo + text message: three userpics).
    let orange = img.pixels().filter(|p| near(p, ORANGE, 10)).count();
    assert!(
        orange > 3 * 150,
        "userpics replace the counts ({orange} px)"
    );

    // Below the photo: a light chosen pill on the chat background, not inside the bubble. Find
    // the lowest row of the photo message's bubble left of the pill and check the pill starts
    // below it.
    let pill_px: Vec<(u32, u32)> = img
        .enumerate_pixels()
        .filter(|(_, _, p)| near(p, [pill.r(), pill.g(), pill.b()], 6))
        .map(|(x, y, _)| (x, y))
        .collect();
    assert!(
        !pill_px.is_empty(),
        "a chosen pill below a bubble is msgServiceFg"
    );
    let pill_top = pill_px.iter().map(|&(_, y)| y).min().unwrap_or(0);
    let pill_left = pill_px.iter().map(|&(x, _)| x).min().unwrap_or(0);
    let column = pill_left + 2;
    let bubble_bottom = (0..pill_top).rev().find(|&y| {
        near(
            img.get_pixel(column, y),
            [bubble_bg.r(), bubble_bg.g(), bubble_bg.b()],
            4,
        )
    });
    let gap_rows = (bubble_bottom.unwrap_or(0) + 1..pill_top)
        .filter(|&y| {
            near(
                img.get_pixel(column, y),
                {
                    let c = colors.canvas_bg;
                    [c.r(), c.g(), c.b()]
                },
                6,
            )
        })
        .count();
    assert!(
        gap_rows >= 2,
        "the chat background shows between the photo's bubble and its reactions"
    );
}
