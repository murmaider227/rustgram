//! UI parity stage 4, package E: the media viewer looks like Telegram Desktop 7.4.6's fullscreen
//! `OverlayWidget` (no top bar, header + name/date bottom-left, title buttons top-right, album
//! strip at the bottom).

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{Pos2, Rect, Vec2};
use egui_kittest::kittest::Queryable;
use image::{ImageFormat, Rgba, RgbaImage};
use tg_audio::{AudioDataSource, AudioMsgId, PlaylistProvider};
use tg_text::TextWithEntities;
use tg_video::RangeLoader;

use super::{MediaOverlayWidget, OverlayEvent};
use crate::media::backend::{
    AudioTrackMetadata, MediaBackend, MediaItem, MediaItemId, MediaItemKind,
};
use crate::media::mock::MockMediaBackend;

const PEER: i64 = 3003;
const ALBUM: i64 = 0x00A1_B0E0;

fn photo(id: u64, grouped_id: Option<i64>, caption: &str) -> MediaItem {
    MediaItem {
        id: MediaItemId(id),
        peer_id: PEER,
        msg_id: id as i32,
        kind: MediaItemKind::Photo,
        width: 800,
        height: 520,
        duration_ms: 0,
        file_size: 88,
        file_name: format!("photo_{id}.jpg"),
        mime_type: "image/jpeg".to_owned(),
        sender_name: "Testname Testsurname".to_owned(),
        date_text: "today at 1:07".to_owned(),
        caption: TextWithEntities {
            text: caption.to_owned(),
            entities: Vec::new(),
        },
        rotation_degrees: 0,
        has_alpha: false,
        allows_forward: true,
        allows_delete: true,
        allows_save: true,
        local_path: None,
        attached_stickers_media: None,
        grouped_id,
    }
}

/// The landscape test picture used by the stage-3 parity account (sky, sun, two mountains).
fn landscape_png(tint: u8) -> Vec<u8> {
    let (w, h) = (800_u32, 520_u32);
    let mut img = RgbaImage::new(w, h);
    for (x, y, px) in img.enumerate_pixels_mut() {
        let t = y as f32 / h as f32;
        let sky = [
            (230.0 + (90.0 - 230.0) * t) as u8,
            (216.0 + (158.0 - 216.0) * t) as u8,
            (220.0 + (236.0 - 220.0) * t) as u8,
        ];
        let sun = ((x as f32 - 620.0).powi(2) + (y as f32 - 120.0).powi(2)).sqrt() < 60.0;
        let left = y as f32 > 250.0 + (x as f32 - 200.0).abs() * 0.9;
        let right = y as f32 > 210.0 + (x as f32 - 560.0).abs() * 0.85;
        let c = if y > 400 {
            [0x1E, 0x3A, 0x5C]
        } else if right {
            [0x26, 0x45, 0x68]
        } else if left {
            [0x34, 0x5E, tint]
        } else if sun {
            [0xFF, 0xDB, 0x70]
        } else {
            sky
        };
        *px = Rgba([c[0], c[1], c[2], 0xFF]);
    }
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, ImageFormat::Png)
        .expect("encode test png");
    out.into_inner()
}

struct ChatBackend {
    items: Vec<MediaItem>,
    inner: MockMediaBackend,
}

impl ChatBackend {
    fn single() -> Self {
        Self {
            items: vec![photo(1, None, "Sample landscape photo")],
            inner: MockMediaBackend::new(),
        }
    }

    fn album() -> Self {
        Self {
            items: vec![
                photo(11, Some(ALBUM), ""),
                photo(12, Some(ALBUM), "Album photo"),
                photo(13, Some(ALBUM), ""),
            ],
            inner: MockMediaBackend::new(),
        }
    }
}

impl MediaBackend for ChatBackend {
    fn get_item(&self, id: MediaItemId) -> Option<MediaItem> {
        self.items.iter().find(|i| i.id == id).cloned()
    }
    fn siblings_in_chat(&self, _peer_id: i64, _item_id: MediaItemId) -> Vec<MediaItem> {
        self.items.clone()
    }
    fn load_bytes(&self, id: MediaItemId) -> anyhow::Result<Vec<u8>> {
        Ok(landscape_png(0x60 + (id.0 % 8) as u8 * 0x18))
    }
    fn open_range_loader(&self, _id: MediaItemId) -> anyhow::Result<Arc<dyn RangeLoader>> {
        anyhow::bail!("no video in this fixture")
    }
    fn save_to_disk(&self, _id: MediaItemId, destination: &Path) -> anyhow::Result<PathBuf> {
        Ok(destination.to_path_buf())
    }
    fn copy_item(&self, _id: MediaItemId) -> anyhow::Result<()> {
        Ok(())
    }
    fn forward_item(&self, _id: MediaItemId) -> anyhow::Result<()> {
        Ok(())
    }
    fn delete_item(&self, _id: MediaItemId) -> anyhow::Result<()> {
        Ok(())
    }
    fn show_in_chat(&self, _peer_id: i64, _msg_id: i32) {}
    fn open_in_external_app(&self, _id: MediaItemId) -> anyhow::Result<PathBuf> {
        Ok(PathBuf::from("x"))
    }
    fn audio_track_metadata(&self, _id: AudioMsgId) -> Option<AudioTrackMetadata> {
        None
    }
    fn audio_data_source(&self) -> Arc<dyn AudioDataSource> {
        self.inner.audio_data_source()
    }
    fn playlist_provider(&self) -> Arc<dyn PlaylistProvider> {
        self.inner.playlist_provider()
    }
}

static WGPU_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn snap_dir() -> PathBuf {
    std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("uip4_e"))
}

struct ViewerState {
    overlay: MediaOverlayWidget,
    backend: ChatBackend,
    now_ms: i64,
    events: Vec<OverlayEvent>,
}

fn harness(
    backend: ChatBackend,
    visits: &[u64],
    size: Vec2,
    now_ms: i64,
) -> egui_kittest::Harness<'static, ViewerState> {
    let mut overlay = MediaOverlayWidget::new();
    let first = visits.first().copied().unwrap_or(1);
    overlay
        .open_item(&backend, PEER, MediaItemId(first), 1_000)
        .expect("open item");
    let state = ViewerState {
        overlay,
        backend,
        now_ms,
        events: Vec::new(),
    };
    let mut harness = egui_kittest::HarnessBuilder::default()
        .with_size(size)
        .with_render_cursor(false)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .wgpu()
        .with_context_setup(|ctx| {
            tg_ui::fonts::register_tdesktop_fonts(ctx);
            ctx.all_styles_mut(|s| s.animation_time = 0.0);
        })
        .build_ui_state(
            move |ui, st: &mut ViewerState| {
                let rect = Rect::from_min_size(Pos2::ZERO, size);
                let events = st.overlay.show(ui.ctx(), rect, &st.backend, st.now_ms);
                st.events.extend(events);
            },
            state,
        );
    harness.run_steps(2);
    // Viewing the other album photos first caches their strip thumbnails.
    for id in visits.iter().skip(1) {
        let st = harness.state_mut();
        st.overlay
            .open_item(&st.backend, PEER, MediaItemId(*id), 1_000)
            .expect("open item");
        harness.run_steps(2);
    }
    harness.run_steps(2);
    harness
}

fn lock() -> std::sync::MutexGuard<'static, ()> {
    WGPU_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[test]
#[ignore = "captures before/after PNGs under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_uip4_e_snapshots() {
    let _guard = lock();
    let stage = std::env::var("RUSTGRAM_UIP4E_SNAP_STAGE").unwrap_or_else(|_| "after".to_owned());
    let dir = snap_dir();
    std::fs::create_dir_all(&dir).expect("create snap dir");
    for (label, size) in [
        ("1440x900", Vec2::new(1440.0, 900.0)),
        ("390x844", Vec2::new(390.0, 844.0)),
    ] {
        let cases: [(&str, ChatBackend, &[u64], i64); 3] = [
            ("media_viewer_photo", ChatBackend::single(), &[1], 1_200),
            (
                "media_viewer_photo_idle",
                ChatBackend::single(),
                &[1],
                6_000,
            ),
            (
                "media_viewer_album",
                ChatBackend::album(),
                &[11, 13, 12],
                1_200,
            ),
        ];
        for (screen, backend, visits, t_ms) in cases {
            let mut h = harness(backend, visits, size, t_ms);
            let img = h.render().expect("wgpu render");
            img.save(dir.join(format!("{screen}_{label}_{stage}.png")))
                .expect("save png");
        }
    }
}

#[test]
fn photo_uses_the_whole_screen_and_footer_names_it() {
    let _guard = lock();
    let h = harness(ChatBackend::single(), &[1], Vec2::new(1440.0, 900.0), 1_200);
    let content = h.state().overlay.zoom_state().content_rect();
    // 800×520 centred in 1440×900: no top bar or bottom bar takes room.
    assert!((content.min.y - 190.0).abs() <= 1.0, "{content:?}");
    assert!((content.min.x - 320.0).abs() <= 1.0, "{content:?}");
    assert!(h.query_by_label("today at 1:07").is_some());
    // Forward lives in the "More" menu, not in the bottom-right buttons.
    assert!(
        h.query_by_label(&tg_lang::tr::lng_mediaview_forward())
            .is_none()
    );
    assert!(h.query_by_label("More").is_some());
    assert!(
        h.query_by_label(&tg_lang::tr::lng_mediaview_save_as())
            .is_some()
    );
}

#[test]
fn title_close_button_closes_the_viewer() {
    let _guard = lock();
    let mut h = harness(ChatBackend::single(), &[1], Vec2::new(1440.0, 900.0), 1_200);
    let close_label = tg_lang::tr::lng_close();
    let close = h.get_by_label(&close_label);
    let rect = close.rect();
    assert!(
        (rect.max.x - 1440.0).abs() <= 1.0 && rect.min.y.abs() <= 1.0,
        "{rect:?}"
    );
    assert!((rect.width() - 44.0).abs() <= 1.0 && (rect.height() - 32.0).abs() <= 1.0);
    close.click();
    h.run_steps(2);
    assert!(!h.state().overlay.is_open());
    assert!(
        h.state()
            .events
            .iter()
            .any(|e| matches!(e, OverlayEvent::Closed))
    );
}

#[test]
fn footer_date_opens_the_message_in_chat() {
    let _guard = lock();
    let mut h = harness(ChatBackend::single(), &[1], Vec2::new(1440.0, 900.0), 1_200);
    let date = h.get_by_label("today at 1:07");
    // `mediaviewTextTop` (26 px) above the bottom edge.
    assert!((date.rect().min.y - (900.0 - 26.0)).abs() <= 1.0);
    date.click();
    h.run_steps(2);
    assert!(!h.state().overlay.is_open());
    assert!(h.state().events.iter().any(|e| matches!(
        e,
        OverlayEvent::ShowInChat {
            peer_id: PEER,
            msg_id: 1
        }
    )));
}

#[test]
fn album_strip_sits_at_the_bottom_with_cached_thumbnails() {
    let _guard = lock();
    let h = harness(
        ChatBackend::album(),
        &[11, 13, 12],
        Vec2::new(1440.0, 900.0),
        1_200,
    );
    let st = h.state();
    let slots = st.overlay.group_thumbs.slots();
    assert_eq!(slots.len(), 3);
    let current = slots[1];
    assert!(current.active);
    assert!((current.rect.min.y - (900.0 - 108.0 + 14.0)).abs() <= 1.0);
    assert!((current.rect.height() - 80.0).abs() <= 1.0);
    assert!((current.rect.center().x - 720.0).abs() <= 1.0);
    for id in [11, 12, 13] {
        assert!(st.overlay.group_thumbs.has_thumb(MediaItemId(id)));
    }
}

#[test]
fn headers_follow_update_header() {
    let single = ChatBackend::single();
    let overlay = MediaOverlayWidget::new();
    assert_eq!(
        overlay.header_text(&single.items[0]),
        tg_lang::tr::lng_mediaview_single_photo()
    );

    let mut video = photo(21, None, "");
    video.kind = MediaItemKind::Video;
    video.file_name = "clip.mp4".to_owned();
    assert_eq!(overlay.header_text(&video), "clip.mp4");
    let mut second = video.clone();
    second.id = MediaItemId(22);
    let nav = super::GalleryNavigator::new(vec![video, second], MediaItemId(21));
    assert_eq!(nav.counter_text().as_deref(), Some("clip.mp4 1 of 2"));
}
