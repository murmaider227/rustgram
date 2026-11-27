//! TGB-3099: a sticker's time is `UnwrappedMedia`'s: to the right of an incoming sticker and
//! only while the pointer is over it (`needInfoDisplay`), at the bottom right of an outgoing one,
//! always for the chat's last own message.
//!
//! Writes `sticker_time_<theme>_<scale>[_hover].png` into `$STICKER_TIME_SNAP_DIR` when set.
//! Mock data only.

use egui::Event;
use egui_kittest::Harness;
use tg_text::TextWithEntities;

use crate::history::backend::{HistoryMedia, HistoryMessage, StickerAttachment};
use crate::history::mock::sample_lottie_star_json;
use crate::history::tests_large_emoji_snap::build_with;
use crate::history::tests_spoiler_snap::{SnapState, message};
use tg_app_core::stand_in::theme::ThemeMode;
use tg_app_core::stand_in::types::{MsgId, PeerId};

fn sticker(peer: PeerId, id: i32, from: i64, name: &str) -> HistoryMessage {
    let mut msg = message(peer, id, from, name, TextWithEntities::default());
    msg.media = Some(HistoryMedia::Sticker(StickerAttachment {
        sticker_id: 9100 + u64::from(id.unsigned_abs()),
        emoji: "\u{2B50}".to_owned(),
        set_title: "Rustgram Vector Pack".to_owned(),
        width: 512,
        height: 512,
        lottie_json: sample_lottie_star_json(),
        set: None,
    }));
    msg
}

fn messages(peer: PeerId) -> Vec<HistoryMessage> {
    let mut out_first = sticker(peer, 2, 702, "Я");
    out_first.outgoing = true;
    let mut out_last = sticker(peer, 4, 702, "Я");
    out_last.outgoing = true;
    vec![
        sticker(peer, 1, 701, "Тестовая Оля"),
        out_first,
        message(
            peer,
            3,
            701,
            "Тестовая Оля",
            TextWithEntities::simple("Класс"),
        ),
        out_last,
    ]
}

fn build(mode: ThemeMode, ppp: f32) -> Harness<'static, SnapState> {
    let peer = PeerId::from_user(88_099);
    build_with(peer, messages(peer), mode, ppp, true)
}

fn hover(harness: &mut Harness<'static, SnapState>, id: i32) {
    let rect = harness
        .state()
        .inner
        .last_painted_bubble_rect(MsgId(id))
        .expect("painted");
    harness
        .input_mut()
        .events
        .push(Event::PointerMoved(rect.center()));
    for _ in 0..3 {
        harness.step();
    }
}

fn layout(harness: &Harness<'static, SnapState>, id: i32) -> tg_history::MessageLayout {
    harness
        .state()
        .inner
        .cached_message_layout(MsgId(id))
        .cloned()
        .expect("laid out")
}

#[test]
fn an_incoming_sticker_keeps_its_time_beside_it() {
    let harness = build(ThemeMode::Day, 1.0);
    let incoming = layout(&harness, 1);
    let sticker = incoming.unwrapped_content_rect;
    assert!(!sticker.is_empty());
    assert!(
        incoming.bottom_info_rect.x >= sticker.x + sticker.width,
        "the pill is right of the sticker: {:?} vs {sticker:?}",
        incoming.bottom_info_rect
    );
    let outgoing = layout(&harness, 2);
    assert_eq!(
        outgoing.bottom_info_rect.x + outgoing.bottom_info_rect.width,
        outgoing.unwrapped_content_rect.x + outgoing.unwrapped_content_rect.width
            - tg_history::consts::MSG_DATE_IMG_DELTA
    );
}

#[test]
fn sticker_time_snapshot() {
    let Some(dir) = std::env::var_os("STICKER_TIME_SNAP_DIR").map(std::path::PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&dir).expect("create snap dir");
    for (theme, mode) in [("day", ThemeMode::Day), ("night", ThemeMode::Night)] {
        for (scale, ppp) in [("100", 1.0), ("150", 1.5)] {
            let mut harness = build(mode, ppp);
            let image = harness.render().expect("render");
            image
                .save(dir.join(format!("sticker_time_{theme}_{scale}.png")))
                .expect("save snapshot");
            hover(&mut harness, 1);
            let image = harness.render().expect("render");
            image
                .save(dir.join(format!("sticker_time_{theme}_{scale}_hover.png")))
                .expect("save snapshot");
        }
    }
}
