//! TGB-3126: a static (WebP) sticker draws its downloaded file, not the blue placeholder
//! circle, and a click shows its pack (`StickerSetBox`) instead of the media viewer, which has
//! no sticker view and showed an error. Mock data only.

use egui::{Event, Modifiers, PointerButton, Pos2, Vec2};
use egui_kittest::Harness;
use tg_text::TextWithEntities;

use crate::history::backend::{HistoryMedia, HistoryMessage, StickerAttachment, StickerSetId};
use crate::history::inner::InnerEvent;
use crate::history::tests_w4_text_snap::{InnerState, group_peer, inner_harness, message};
use tg_app_core::core_types::dto::inline_media::{InlineMediaDescriptor, PhotoLoadState};
use tg_app_core::stand_in::types::MsgId;

const SET: StickerSetId = StickerSetId {
    id: 88_001,
    access_hash: 99_002,
};

fn sticker(id: i32, set: Option<StickerSetId>) -> HistoryMessage {
    let mut msg = message(group_peer(), id, 1002, "Ivo Marten", "");
    msg.text = TextWithEntities::default();
    msg.media = Some(HistoryMedia::Sticker(StickerAttachment {
        sticker_id: 7000 + u64::from(id.unsigned_abs()),
        emoji: "\u{1F600}".to_owned(),
        set_title: String::new(),
        width: 512,
        height: 512,
        // A static sticker: no Lottie source, its file comes through the photo pipeline.
        lottie_json: String::new(),
        set,
    }));
    msg
}

/// A 64 px opaque red square as PNG (the sticker file the session downloaded).
fn red_png() -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(64, 64, image::Rgba([0xE0, 0x20, 0x20, 0xFF]));
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .expect("encode png");
    out
}

fn harness() -> Harness<'static, InnerState> {
    let mut harness = inner_harness(
        vec![sticker(1, Some(SET)), sticker(2, None)],
        Vec2::new(720.0, 640.0),
    );
    harness.state_mut().store.set_media_descriptor(
        group_peer(),
        MsgId(1),
        InlineMediaDescriptor {
            photo_load: Some(PhotoLoadState::Ready {
                bytes: red_png().into(),
            }),
            ..InlineMediaDescriptor::default()
        },
    );
    harness.state_mut().store.set_media_descriptor(
        group_peer(),
        MsgId(2),
        InlineMediaDescriptor {
            photo_load: Some(PhotoLoadState::Pending),
            ..InlineMediaDescriptor::default()
        },
    );
    // The list asks for each message's media when its page arrives (`request_media_descriptor`).
    let state = harness.state_mut();
    for id in [1, 2] {
        state
            .inner
            .request_media_descriptor(group_peer(), MsgId(id), &state.store);
    }
    harness
}

fn sticker_centre(harness: &Harness<'static, InnerState>, id: i32) -> Pos2 {
    let inner = &harness.state().inner;
    let painted = inner.last_painted_bubble_rect(MsgId(id)).expect("painted");
    let layout = inner.cached_message_layout(MsgId(id)).expect("laid out");
    let content = layout.unwrapped_content_rect;
    painted.min
        + Vec2::new(
            (content.x - layout.geometry.x + content.width / 2) as f32,
            (content.y - layout.geometry.y + content.height / 2) as f32,
        )
}

#[test]
fn a_static_sticker_draws_its_file_not_the_placeholder() {
    let mut harness = harness();
    // The file decodes on a worker thread: give it a few frames.
    let mut px = image::Rgba([0, 0, 0, 0]);
    for _ in 0..40 {
        harness.step();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let at = sticker_centre(&harness, 1);
        let image = harness.render().expect("render");
        px = *image.get_pixel(at.x as u32, at.y as u32);
        if px[0] > 200 && px[2] < 80 {
            break;
        }
    }
    let at_loading = sticker_centre(&harness, 2);
    let image = harness.render().expect("render");
    if let Some(dir) = std::env::var_os("STATIC_STICKER_SNAP_DIR").map(std::path::PathBuf::from) {
        std::fs::create_dir_all(&dir).expect("create snap dir");
        image
            .save(dir.join("static_sticker_day.png"))
            .expect("save snapshot");
    }
    assert!(
        px[0] > 200 && px[1] < 80 && px[2] < 80,
        "the red file is drawn: {px:?}"
    );
    // Still loading: nothing at all, never the blue placeholder circle.
    let loading = *image.get_pixel(at_loading.x as u32, at_loading.y as u32);
    assert!(
        !(loading[2] > 150 && loading[0] < 120),
        "no blue placeholder while the file loads: {loading:?}"
    );
}

fn click(harness: &mut Harness<'static, InnerState>, pos: Pos2) {
    harness.input_mut().events.push(Event::PointerMoved(pos));
    harness.step();
    for pressed in [true, false] {
        harness.input_mut().events.push(Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
        harness.step();
    }
}

#[test]
fn a_click_on_a_sticker_opens_its_pack_not_the_media_viewer() {
    let mut harness = harness();
    harness.state_mut().events.clear();
    let at = sticker_centre(&harness, 1);
    click(&mut harness, at);
    let events = &harness.state().events;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, InnerEvent::OpenStickerSet(set) if *set == SET)),
        "{events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, InnerEvent::OpenMedia { .. })),
        "{events:?}"
    );

    // A sticker without a set does nothing on a click (`Sticker::link` is empty).
    harness.state_mut().events.clear();
    let at = sticker_centre(&harness, 2);
    click(&mut harness, at);
    let events = &harness.state().events;
    assert!(
        !events.iter().any(|e| matches!(
            e,
            InnerEvent::OpenStickerSet(_) | InnerEvent::OpenMedia { .. }
        )),
        "{events:?}"
    );
}
