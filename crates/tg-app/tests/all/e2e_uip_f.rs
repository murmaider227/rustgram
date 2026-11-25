//! UI parity stage 2, package F: before/after snapshots of live-converted chat-list rows and the
//! media viewer, built from a synthetic `tg_data::Session` (no network, no real account).
//!
//! Run with `RUSTGRAM_SNAPSHOT_DIR=<dir> RUSTGRAM_UIPF_SNAP_STAGE=before|after cargo test -p tg-app
//! --test all e2e_uip_f -- --ignored`.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use egui::{Pos2, Rect, Vec2};
use image::{ImageFormat, Rgba, RgbaImage};
use tg_app::media::backend::{MediaBackend, MediaItem, MediaItemId};
use tg_app::media::mock::MockMediaBackend;
use tg_app::media::overlay::MediaOverlayWidget;
use tg_app_core::dialogs::backend::{DialogItem, DialogListQuery};
use tg_app_core::stand_in::theme::Palette;
use tg_app_core::stand_in::types::FilterId;
use tg_app_info::dialogs::layout::{DialogColors, RowPaintState, paint_dialog_row};
use tg_data::{
    Channel, ChannelFlags, ChannelId, Chat, ChatId, DocumentData, DocumentKind, LastseenStatus,
    Message, MessageContent, MessageFlags, MessageMedia, MsgId, PeerId, PeerUpdateFlags, PhotoData,
    PhotoSize, ServiceAction, Session, TextWithEntities, User, UserId,
};
use tg_history::LocalDateTime;

use crate::common::{apply_deterministic_style, init_headless_env};

static WGPU_UIPF_LOCK: Mutex<()> = Mutex::new(());

const NOW: i32 = 1_764_000_000;
const SELF_ID: u64 = 1001;
const CONTACT_ID: u64 = 1002;
const BOT_ID: u64 = 1003;
const GROUP_ID: u64 = 60;
const CHANNEL_ID: u64 = 50;
const PHOTO_ID: u64 = 880_001;

fn user(id: u64, is_self: bool, first: &str, last: &str) -> User {
    let mut u = User::new(UserId(id), is_self);
    u.set_name(first, last, "", "");
    u.lastseen = LastseenStatus::online_till_status(NOW + 300, false, false);
    u
}

fn msg(peer: PeerId, id: i64, from: Option<PeerId>, date: i32, text: &str) -> Message {
    Message::new_text(peer, MsgId(id), from, date, TextWithEntities::plain(text))
}

fn with_media(mut m: Message, caption: &str, media: MessageMedia) -> Message {
    m.content = MessageContent::Regular {
        text: TextWithEntities::plain(caption),
        media,
    };
    m
}

fn landscape_photo() -> MessageMedia {
    MessageMedia::Photo(Box::new(PhotoData {
        id: tg_data::PhotoId(PHOTO_ID),
        sizes: vec![PhotoSize {
            width: 800,
            height: 520,
            ..Default::default()
        }],
        ..Default::default()
    }))
}

fn notes_file() -> MessageMedia {
    MessageMedia::Document(Box::new(DocumentData {
        id: tg_data::DocumentId(880_002),
        kind: DocumentKind::File,
        file_name: "sample_notes.txt".to_owned(),
        mime_type: "text/plain".to_owned(),
        size: 88,
        ..Default::default()
    }))
}

/// The parity test account layout: a contact, a group, a channel, Saved Messages and a bot.
fn parity_session() -> Session {
    let mut s = Session::new(UserId(SELF_ID));
    let me = PeerId::from_user(UserId(SELF_ID));
    s.upsert_user(
        user(SELF_ID, true, "Testname", "Testsurname"),
        PeerUpdateFlags::all(),
    );
    s.upsert_user(
        user(CONTACT_ID, false, "Sample", "Contact"),
        PeerUpdateFlags::all(),
    );
    let mut bot = user(BOT_ID, false, "Suggested Attach Bot", "");
    bot.bot_info = Some(Default::default());
    s.upsert_user(bot, PeerUpdateFlags::all());

    let mut chat = Chat::new(ChatId(GROUP_ID));
    chat.set_name("Parity Sample Group");
    chat.creator = UserId(SELF_ID);
    chat.count = 1;
    s.upsert_chat(chat, PeerUpdateFlags::all());

    let mut ch = Channel::new(ChannelId(CHANNEL_ID));
    ch.flags.remove(ChannelFlags::FORBIDDEN);
    ch.set_name("Parity Sample Channel", "");
    ch.flags
        .insert(ChannelFlags::BROADCAST | ChannelFlags::CREATOR);
    s.upsert_channel(ch, PeerUpdateFlags::all());

    let contact = PeerId::from_user(UserId(CONTACT_ID));
    s.insert_message(msg(
        contact,
        1,
        Some(contact),
        NOW - 600,
        "Hello everyone from Account B!",
    ));

    let group = PeerId::from_chat(ChatId(GROUP_ID));
    let mut welcome = msg(
        group,
        2,
        Some(me),
        NOW - 700,
        "Welcome to Parity Sample Group! Checking group chat layout and member list.",
    );
    welcome.flags.insert(MessageFlags::OUTGOING);
    s.insert_message(welcome);

    let channel = PeerId::from_channel(ChannelId(CHANNEL_ID));
    let mut post = with_media(
        msg(channel, 3, None, NOW - 800, ""),
        "Channel photo post",
        landscape_photo(),
    );
    post.flags
        .insert(MessageFlags::POST | MessageFlags::OUTGOING);
    s.insert_message(post);

    s.insert_message(with_media(
        msg(me, 4, Some(me), NOW - 900, ""),
        "Sample landscape photo",
        landscape_photo(),
    ));
    s.insert_message(with_media(
        msg(me, 5, Some(me), NOW - 850, ""),
        "",
        notes_file(),
    ));

    let bot_peer = PeerId::from_user(UserId(BOT_ID));
    let mut allowed = msg(bot_peer, 6, Some(bot_peer), NOW - 1000, "");
    allowed.content = MessageContent::Service(Box::new(ServiceAction::BotAllowed {
        attach_menu: true,
        from_request: false,
        domain: None,
        app_id: None,
    }));
    s.insert_message(allowed);
    s
}

fn dialog_rows(s: &Session) -> Vec<DialogItem> {
    [
        PeerId::from_user(UserId(CONTACT_ID)),
        PeerId::from_chat(ChatId(GROUP_ID)),
        PeerId::from_channel(ChannelId(CHANNEL_ID)),
        PeerId::from_user(UserId(SELF_ID)),
        PeerId::from_user(UserId(BOT_ID)),
    ]
    .into_iter()
    .filter_map(|pid| {
        tg_app_live::convert::dialog_item_from_session(
            s,
            pid,
            DialogListQuery::Filter(FilterId::ALL),
            NOW,
        )
    })
    .collect()
}

/// Media backend over the session's Saved Messages: siblings and items come from the live
/// conversion; the photo bytes are a synthetic landscape.
struct SessionMediaBackend {
    items: Vec<MediaItem>,
    photo_png: Vec<u8>,
    fallback: MockMediaBackend,
}

impl SessionMediaBackend {
    fn new(s: &Session) -> Self {
        let me = PeerId::from_user(UserId(SELF_ID));
        let items = [4, 5]
            .into_iter()
            .filter_map(|id| s.message(tg_data::FullMsgId::new(me, MsgId(id))))
            .filter_map(|m| tg_app_live::convert::message_to_viewer_media_item(s, m))
            .collect();
        Self {
            items,
            photo_png: landscape_png(),
            fallback: MockMediaBackend::new(),
        }
    }
}

fn landscape_png() -> Vec<u8> {
    let (w, h) = (800_u32, 520_u32);
    let mut img = RgbaImage::new(w, h);
    for (x, y, px) in img.enumerate_pixels_mut() {
        let t = y as f32 / h as f32;
        let sky = [
            (0xE6 as f32 + (0x5A as f32 - 0xE6 as f32) * t) as u8,
            (0xD8 as f32 + (0x9E as f32 - 0xD8 as f32) * t) as u8,
            (0xDC as f32 + (0xEC as f32 - 0xDC as f32) * t) as u8,
        ];
        let sun = ((x as f32 - 620.0).powi(2) + (y as f32 - 120.0).powi(2)).sqrt() < 60.0;
        let left = y as f32 > 250.0 + (x as f32 - 200.0).abs() * 0.9;
        let right = y as f32 > 210.0 + (x as f32 - 560.0).abs() * 0.85;
        let ground = y > 400;
        let c = if ground {
            [0x1E, 0x3A, 0x5C]
        } else if right {
            [0x26, 0x45, 0x68]
        } else if left {
            [0x34, 0x5E, 0x80]
        } else if sun {
            [0xFF, 0xDB, 0x70]
        } else {
            sky
        };
        *px = Rgba([c[0], c[1], c[2], 0xFF]);
    }
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, ImageFormat::Png)
        .expect("encode png");
    out.into_inner()
}

impl MediaBackend for SessionMediaBackend {
    fn get_item(&self, id: MediaItemId) -> Option<MediaItem> {
        self.items.iter().find(|i| i.id == id).cloned()
    }
    fn siblings_in_chat(&self, _peer_id: i64, _item_id: MediaItemId) -> Vec<MediaItem> {
        self.items.clone()
    }
    fn load_bytes(&self, _id: MediaItemId) -> anyhow::Result<Vec<u8>> {
        Ok(self.photo_png.clone())
    }
    fn open_range_loader(
        &self,
        _id: MediaItemId,
    ) -> anyhow::Result<Arc<dyn tg_video::RangeLoader>> {
        anyhow::bail!("no video in this fixture")
    }
    fn save_to_disk(&self, _id: MediaItemId, dest: &Path) -> anyhow::Result<PathBuf> {
        Ok(dest.to_path_buf())
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
        Ok(PathBuf::from("photo.jpg"))
    }
    fn audio_track_metadata(
        &self,
        _id: tg_audio::AudioMsgId,
    ) -> Option<tg_app::media::backend::AudioTrackMetadata> {
        None
    }
    fn audio_data_source(&self) -> Arc<dyn tg_audio::AudioDataSource> {
        self.fallback.audio_data_source()
    }
    fn playlist_provider(&self) -> Arc<dyn tg_audio::PlaylistProvider> {
        self.fallback.playlist_provider()
    }
}

fn snap_dir() -> PathBuf {
    std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("uip2_f"))
}

fn save(img: &image::RgbaImage, path: &Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create snap dir");
    }
    img.save(path).expect("save png");
}

fn harness_builder<S>(size: Vec2) -> egui_kittest::HarnessBuilder<S> {
    init_headless_env();
    egui_kittest::HarnessBuilder::<S>::default()
        .with_size(size)
        .with_render_cursor(false)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .wgpu()
        .with_context_setup(|ctx| {
            tg_ui::fonts::register_tdesktop_fonts(ctx);
            apply_deterministic_style(ctx);
        })
}

#[test]
#[ignore = "captures before/after PNGs under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_uip_f_snapshots() {
    let _guard = WGPU_UIPF_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stage = std::env::var("RUSTGRAM_UIPF_SNAP_STAGE").unwrap_or_else(|_| "after".to_owned());
    let dir = snap_dir();
    let session = parity_session();
    let rows = dialog_rows(&session);
    assert_eq!(rows.len(), 5, "all five parity chats convert");

    for (size_label, size) in [
        ("1440x900", Vec2::new(1440.0, 900.0)),
        ("390x844", Vec2::new(390.0, 844.0)),
    ] {
        // Chat list rows (the left column at its official 1440 px width, or the full phone width).
        let column_w = size.x.min(445.0);
        let rows_for_ui = rows.clone();
        let mut h = harness_builder(size).build_ui(move |ui| {
            let palette = Palette::day();
            let colors = DialogColors::new(&palette);
            let painter = ui.painter().clone();
            painter.rect_filled(ui.max_rect(), 0.0, palette.window_bg);
            let now = LocalDateTime::new(2026, 11, 25, 0, 10, 0);
            for (i, item) in rows_for_ui.iter().enumerate() {
                let rect = Rect::from_min_size(
                    Pos2::new(0.0, 8.0 + i as f32 * 62.0),
                    Vec2::new(column_w, 62.0),
                );
                let _ = paint_dialog_row(
                    &painter,
                    rect,
                    item,
                    now,
                    RowPaintState::default(),
                    &palette,
                    &colors,
                );
            }
        });
        h.run_steps(3);
        let img = h.render().expect("wgpu render");
        save(
            &img,
            &dir.join(format!("uipf_chat_list_rows_{size_label}_{stage}.png")),
        );

        // Media viewer opened on the Saved Messages photo: just opened, then idle for 3 s.
        for (moment, t_ms) in [("open", 1_200_i64), ("idle3s", 4_200_i64)] {
            let backend = SessionMediaBackend::new(&session);
            let photo_item = MediaItemId(PHOTO_ID);
            let me = PeerId::from_user(UserId(SELF_ID)).0;
            let mut overlay = MediaOverlayWidget::new();
            overlay
                .open_item(&backend, me, photo_item, 1_000)
                .expect("open saved photo");
            let mut h = harness_builder(size).build_ui_state(
                move |ui, (ov, bk): &mut (MediaOverlayWidget, SessionMediaBackend)| {
                    let vp = ui.ctx().content_rect();
                    let _ = ov.show(ui.ctx(), vp, bk, t_ms);
                },
                (overlay, backend),
            );
            h.run_steps(4);
            let img = h.render().expect("wgpu render");
            save(
                &img,
                &dir.join(format!(
                    "uipf_media_viewer_{moment}_{size_label}_{stage}.png"
                )),
            );
        }
    }
}
