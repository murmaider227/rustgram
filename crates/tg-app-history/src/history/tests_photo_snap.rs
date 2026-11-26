//! Screenshot harness for photo bubbles (TGB-2970, synthetic data): a forwarded tall phone
//! screenshot with a short caption, and a loading photo painted twice half a second apart.
//!
//! Writes PNGs into `$PHOTO_SNAP_DIR` when that variable is set; otherwise it only renders the
//! frames (a smoke test for the painter).

use std::sync::Arc;

use egui::{Pos2, Rect, Vec2};
use egui_kittest::{Harness, HarnessBuilder};
use image::{ImageFormat, Rgba, RgbaImage};
use tg_media::image::inline::encode_stripped_thumb;
use tg_text::TextWithEntities;
use tg_ui::fonts::register_tdesktop_fonts;

use crate::history::backend::{
    ForwardHeader, HistoryMedia, HistoryMessage, HistoryPeerInfo, HistorySlice, MessageExtra,
    PhotoAttachment,
};
use crate::history::inline_media::{InlineMediaDescriptor, PhotoLoadState};
use crate::history::inner::HistoryInnerWidget;
use crate::history::mock::MockHistoryStore;
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind, PeerSummary};

const BASE_UNIX: i32 = 1_795_340_000;

pub(crate) struct PhotoSnapState {
    pub(crate) inner: HistoryInnerWidget,
    pub(crate) store: MockHistoryStore,
    peer_info: HistoryPeerInfo,
    list_rect: Rect,
    colors: HistoryColors,
}

/// A phone-screenshot-like picture: light page, dark status bar and round dots in a grid, so
/// any stretch shows as ovals.
fn tall_screenshot(width: u32, height: u32) -> RgbaImage {
    let cell = width / 4;
    let radius = (cell as f32) * 0.36;
    RgbaImage::from_fn(width, height, |x, y| {
        if y < height / 24 {
            return Rgba([30, 34, 40, 255]);
        }
        let cx = (x % cell) as f32 - cell as f32 / 2.0;
        let cy = (y % cell) as f32 - cell as f32 / 2.0;
        if cx.hypot(cy) <= radius {
            Rgba([0x3B, 0x8E, 0xC4, 255])
        } else {
            Rgba([0xF4, 0xF4, 0xF6, 255])
        }
    })
}

fn png(img: &RgbaImage) -> Arc<[u8]> {
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(img.clone())
        .to_rgb8()
        .write_to(&mut std::io::Cursor::new(&mut out), ImageFormat::Png)
        .expect("encode png");
    out.into()
}

/// The stripped (inline) thumbnail Telegram sends with a photo: a ~40 px version of it.
fn stripped(img: &RgbaImage) -> Vec<u8> {
    let (w, h) = (img.width(), img.height());
    let scale = 40.0 / w.max(h) as f32;
    let small = image::imageops::resize(
        img,
        ((w as f32 * scale).round() as u32).max(1),
        ((h as f32 * scale).round() as u32).max(1),
        image::imageops::FilterType::Triangle,
    );
    encode_stripped_thumb(&small)
}

pub(crate) fn photo_message(
    peer: PeerId,
    id: i32,
    size: (i32, i32),
    thumb: &RgbaImage,
    caption: &str,
) -> HistoryMessage {
    HistoryMessage {
        id: MsgId(id),
        peer_id: peer,
        from_id: PeerId::from_user(7001),
        sender_name: "Sample Sender".to_owned(),
        date_unix: BASE_UNIX + id * 20,
        outgoing: false,
        is_post: false,
        text: TextWithEntities::simple(caption),
        media: Some(HistoryMedia::Photo(PhotoAttachment {
            width: size.0,
            height: size.1,
            stripped_thumb: Some(stripped(thumb)),
            fallback_rgb: ([0x3B, 0x8E, 0xC4], [0x1C, 0x4E, 0x78]),
            file_name: "screenshot.png".to_owned(),
            size_bytes: 248_910,
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
    }
}

/// The owner's case: a forwarded 1080x2340 phone screenshot with a short caption.
pub(crate) fn tall_forwarded(peer: PeerId, id: i32) -> (HistoryMessage, InlineMediaDescriptor) {
    let picture = tall_screenshot(540, 1170);
    let mut msg = photo_message(peer, id, (1080, 2340), &picture, "Order 2045159");
    msg.forward = Some(ForwardHeader {
        from_name: "Sample Friend".to_owned(),
        from_peer: None,
        original_date: String::new(),
    });
    let desc = InlineMediaDescriptor {
        photo_load: Some(PhotoLoadState::Ready {
            bytes: png(&picture),
        }),
        ..InlineMediaDescriptor::default()
    };
    (msg, desc)
}

fn peer_info(peer: PeerId) -> HistoryPeerInfo {
    HistoryPeerInfo {
        summary: PeerSummary {
            id: peer,
            kind: PeerKind::User,
            title: "Snapshot".to_owned(),
            status: String::new(),
            username: None,
            phone_or_link: None,
            about: String::new(),
            initials: "SN".to_owned(),
            avatar_rgb: [0x6C, 0x9F, 0xE0],
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

/// A chat with `messages`, their media descriptors registered on the media runtime.
pub(crate) fn build_photo_harness(
    peer: PeerId,
    messages: Vec<HistoryMessage>,
    descriptors: Vec<(MsgId, InlineMediaDescriptor)>,
    size: Vec2,
    wgpu: bool,
) -> Harness<'static, PhotoSnapState> {
    build_photo_harness_with(peer, messages, descriptors, size, wgpu, |_| {})
}

/// [`build_photo_harness`] with `setup` run on the mock store before the first frame.
pub(crate) fn build_photo_harness_with(
    peer: PeerId,
    messages: Vec<HistoryMessage>,
    descriptors: Vec<(MsgId, InlineMediaDescriptor)>,
    size: Vec2,
    wgpu: bool,
    setup: impl FnOnce(&mut MockHistoryStore),
) -> Harness<'static, PhotoSnapState> {
    let mut inner = HistoryInnerWidget::new();
    let total_count = messages.len();
    let mut store = MockHistoryStore::new();
    setup(&mut store);
    for (id, desc) in &descriptors {
        store.set_media_descriptor(peer, *id, desc.clone());
    }
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
    for (id, desc) in descriptors {
        inner.media_runtime_mut().register_descriptor(id, desc);
    }
    let state = PhotoSnapState {
        inner,
        store,
        peer_info: peer_info(peer),
        list_rect: Rect::from_min_size(Pos2::ZERO, size),
        colors: HistoryColors::from_palette(&Palette::for_mode(ThemeMode::Day)),
    };
    let mut builder = HarnessBuilder::default()
        .with_size(size)
        .with_step_dt(0.05)
        .with_max_steps(8)
        .allow_missing_glyphs()
        .with_accessibility_check(false);
    if wgpu {
        builder = builder.wgpu();
    }
    builder
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
        })
        .build_ui_state(
            |ui, st: &mut PhotoSnapState| {
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
        )
}

/// Steps until the decode pool has landed the full photo and its blur-up finished.
pub(crate) fn settle(harness: &mut Harness<'static, PhotoSnapState>) {
    for _ in 0..24 {
        harness.step();
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

#[test]
fn photo_bubbles_snapshot() {
    let dir = std::env::var_os("PHOTO_SNAP_DIR").map(std::path::PathBuf::from);
    let want = dir.is_some();
    let peer = PeerId::from_user(7001);
    let mut shots: Vec<(&str, RgbaImage)> = Vec::new();

    let (tall, tall_desc) = tall_forwarded(peer, 1);
    let mut harness = build_photo_harness(
        peer,
        vec![tall],
        vec![(MsgId(1), tall_desc)],
        Vec2::new(640.0, 600.0),
        want,
    );
    settle(&mut harness);
    if want {
        shots.push(("tall_forwarded", harness.render().expect("render")));
    }

    // A photo that is still downloading, with the progress standing still.
    let landscape = RgbaImage::from_fn(64, 41, |x, y| {
        Rgba([0x3B + (x as u8), 0x6E + (y as u8), 0xA4, 255])
    });
    let loading = photo_message(peer, 2, (1280, 820), &landscape, "");
    let loading_desc = InlineMediaDescriptor {
        photo_load: Some(PhotoLoadState::Loading {
            progress: Some(0.3),
            bytes_loaded: 74_000,
            total_bytes: 248_910,
        }),
        ..InlineMediaDescriptor::default()
    };
    let mut harness = build_photo_harness(
        peer,
        vec![loading],
        vec![(MsgId(2), loading_desc)],
        Vec2::new(640.0, 400.0),
        want,
    );
    settle(&mut harness);
    if want {
        shots.push(("loading_t0", harness.render().expect("render")));
    }
    for _ in 0..10 {
        harness.step();
    }
    if want {
        shots.push(("loading_t500ms", harness.render().expect("render")));
    }

    if let Some(dir) = dir {
        std::fs::create_dir_all(&dir).expect("create snap dir");
        for (name, img) in shots {
            img.save(dir.join(format!("{name}.png")))
                .expect("save snapshot");
        }
    }
}
