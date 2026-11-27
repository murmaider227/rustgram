//! TGB-3097: messages of only custom emoji are drawn large without a bubble
//! (`HistoryView::CustomEmoji`), and a custom emoji among 1-3 emoji is a `LargeEmoji` item.
//!
//! Writes `custom_emoji_<theme>_<scale>.png` into `$CUSTOM_EMOJI_SNAP_DIR` when set. The custom
//! emoji are synthetic shapes registered in the shared renderer; mock data only.

use egui::Vec2;
use egui_kittest::Harness;
use tg_text::{EntityInText, EntityType, TextWithEntities};
use tg_ui::custom_emoji::{CustomEmojiAsset, register_custom_emoji_asset};

use crate::history::backend::HistoryMessage;
use crate::history::tests_large_emoji_snap::build_with;
use crate::history::tests_spoiler_snap::{SnapState, message};
use tg_app_core::stand_in::theme::ThemeMode;
use tg_app_core::stand_in::types::{MsgId, PeerId};

const HEART: &str = "930970001";
const STAR: &str = "930970002";

/// A 64 px synthetic shape: a red heart or a blue five-point star.
fn shape(star: bool) -> CustomEmojiAsset {
    let side = 64u32;
    let mut rgba = Vec::with_capacity((side * side * 4) as usize);
    for y in 0..side {
        for x in 0..side {
            let fx = (x as f32 / side as f32 - 0.5) * 2.6;
            let fy = (0.5 - y as f32 / side as f32) * 2.6 + 0.2;
            let inside = if star {
                let (r, a) = (fx.hypot(fy - 0.2), fy.atan2(fx) * 2.5);
                r < 0.55 + 0.45 * a.cos().abs()
            } else {
                (fx * fx + fy * fy - 1.0).powi(3) - fx * fx * fy.powi(3) <= 0.0
            };
            rgba.extend_from_slice(if !inside {
                &[0, 0, 0, 0]
            } else if star {
                &[0x3A, 0x7B, 0xE5, 0xFF]
            } else {
                &[0xE5, 0x3A, 0x5C, 0xFF]
            });
        }
    }
    CustomEmojiAsset::static_rgba(side, side, rgba)
}

fn register_shapes() {
    assert!(register_custom_emoji_asset(HEART, &shape(false)));
    assert!(register_custom_emoji_asset(STAR, &shape(true)));
}

/// `ids` as custom emoji over "😀" placeholders, `\n` between lines.
fn custom_text(lines: &[&[&str]]) -> TextWithEntities {
    let mut text = String::new();
    let mut entities = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            text.push('\n');
        }
        for id in *line {
            let offset = i32::try_from(text.encode_utf16().count()).unwrap_or(i32::MAX);
            text.push('\u{1F600}');
            entities.push(EntityInText::with_data(
                EntityType::CustomEmoji,
                offset,
                2,
                *id,
            ));
        }
    }
    TextWithEntities { text, entities }
}

fn messages(peer: PeerId) -> Vec<HistoryMessage> {
    let mixed = TextWithEntities {
        text: "\u{1F525}\u{1F600}".to_owned(),
        entities: vec![EntityInText::with_data(
            EntityType::CustomEmoji,
            2,
            2,
            HEART,
        )],
    };
    let mut list = vec![
        message(
            peer,
            1,
            701,
            "Тестовая Оля",
            TextWithEntities::simple("Смотри"),
        ),
        message(peer, 2, 701, "Тестовая Оля", custom_text(&[&[HEART]])),
        message(peer, 3, 701, "Тестовая Оля", custom_text(&[&[HEART, STAR]])),
        message(peer, 4, 702, "Я", mixed),
        message(
            peer,
            5,
            702,
            "Я",
            custom_text(&[&[STAR, HEART, STAR, HEART], &[HEART]]),
        ),
    ];
    for msg in &mut list[3..] {
        msg.outgoing = true;
    }
    list
}

fn build(mode: ThemeMode, ppp: f32) -> Harness<'static, SnapState> {
    build_large(mode, ppp, true)
}

fn build_large(mode: ThemeMode, ppp: f32, large_emoji: bool) -> Harness<'static, SnapState> {
    register_shapes();
    let peer = PeerId::from_user(88_097);
    let mut harness = build_with(peer, messages(peer), mode, ppp, large_emoji);
    for _ in 0..4 {
        harness.step();
    }
    harness
}

#[test]
fn custom_emoji_only_messages_are_large_without_a_bubble() {
    let harness = build(ThemeMode::Day, 1.0);
    let inner = &harness.state().inner;
    let layout = |id| inner.cached_message_layout(MsgId(id)).expect("laid out");
    for id in [2, 3, 4, 5] {
        assert!(!layout(id).has_bubble, "message {id} is emoji only");
        assert!(layout(id).unwrapped);
    }
    assert!(layout(1).has_bubble);
    // One custom emoji is the 112 px emoji sticker, two are 78 px each, a 4 + 1 grid 43 px.
    assert!(
        layout(2).media_rect.height >= 112,
        "{:?}",
        layout(2).media_rect
    );
    assert!(layout(3).media_rect.height >= 78);
    assert!(layout(5).media_rect.height >= 2 * 43);

    // The heart of message 2 is painted where its 112 px cell is.
    let painted = inner.last_painted_bubble_rect(MsgId(2)).expect("painted");
    let l = layout(2);
    let inner_rect =
        tg_history::unwrapped_content_rect(l.media_rect, tg_history::Size::new(112, 112), false);
    let centre = painted.min
        + Vec2::new(
            (inner_rect.x - l.geometry.x) as f32 + 56.0,
            (inner_rect.y - l.geometry.y) as f32 + 60.0,
        );
    let mut harness = harness;
    let image = harness.render().expect("render");
    let px = image.get_pixel(centre.x as u32, centre.y as u32);
    assert!(px[0] > 180 && px[1] < 120, "the heart is drawn: {px:?}");
}

#[test]
fn custom_emoji_snapshot() {
    let Some(dir) = std::env::var_os("CUSTOM_EMOJI_SNAP_DIR").map(std::path::PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&dir).expect("create snap dir");
    for (theme, mode) in [("day", ThemeMode::Day), ("night", ThemeMode::Night)] {
        for (scale, ppp) in [("100", 1.0), ("150", 1.5)] {
            // "off" draws every message as text: the path custom emoji took before TGB-3097.
            for (tag, on) in [("on", true), ("off", false)] {
                let mut harness = build_large(mode, ppp, on);
                let image = harness.render().expect("render");
                image
                    .save(dir.join(format!("custom_emoji_{theme}_{scale}_{tag}.png")))
                    .expect("save snapshot");
            }
        }
    }
}
