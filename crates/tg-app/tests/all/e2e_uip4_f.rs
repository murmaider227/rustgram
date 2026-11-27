//! UI parity stage 4, package F: chat-list rows, search results and the chat-list menu.
//!
//! The rows come from a synthetic `tg_data::Session` shaped like the stage-3 test account (no
//! network, no real account) and go through the live `dialog_item_from_session` conversion.
//! Run with `RUSTGRAM_SNAPSHOT_DIR=<dir> RUSTGRAM_UIP4F_SNAP_STAGE=before|after cargo test -p
//! tg-app --test all e2e_uip4_f -- --ignored`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use egui::{Pos2, Rect, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use image::{Rgba, RgbaImage};
use tg_app::dialogs::backend::{
    DialogItem, DialogListQuery, DialogPeerKind, DialogsBackend, MessageSearchResult,
    SearchResults, SpecialRowKind,
};
use tg_app::dialogs::layout::{DialogColors, RowPaintState, paint_dialog_row};
use tg_app::dialogs::mock::MockDialogsStore;
use tg_app::stand_in::types::{FilterId, PeerId as AppPeerId};
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_info::dialogs::search::{DialogsSearchState, show_search_body};
use tg_data::{
    Channel, ChannelFlags, ChannelId, Chat, ChatId, DocumentData, DocumentKind, LastseenStatus,
    Message, MessageContent, MessageFlags, MessageMedia, MsgId, PeerId, PeerUpdateFlags, PhotoData,
    PhotoSize, ServiceAction, Session, TextWithEntities, User, UserId,
};
use tg_history::{DialogSendState, LocalDateTime};

use crate::common::{
    AppTestState, DEFAULT_MAX_FRAMES, apply_deterministic_style, build_wgpu_harness, hover_at,
    init_headless_env, right_click_at, settle_layout, step_frames, type_text, wait_for_menu_item,
    wait_until,
};

static WGPU_UIP4F_LOCK: Mutex<()> = Mutex::new(());

/// 2026-11-25 03:07 at UTC+2 (the stage-3 capture's local clock).
const T_307: i32 = 1_795_568_820;
const SELF_ID: u64 = 1001;
const CONTACT_ID: u64 = 1002;
const BOT_ID: u64 = 1003;
const GROUP_ID: u64 = 60;
const CHANNEL_ID: u64 = 50;
/// Width of the official chat-list column at 1440x900 in the stage-3 Day chat list (01).
const OFFICIAL_COLUMN_W: f32 = 642.0;
/// The same column in the stage-3 Night chat list (24) and search (18) captures.
const OFFICIAL_COLUMN_W_WIDE: f32 = 738.0;
const ROW_H: f32 = 62.0;
const DIALOGS_TOP_BAR_H: f32 = 54.0;

fn now_local() -> LocalDateTime {
    LocalDateTime::new(2026, 11, 25, 3, 19, 0)
}

fn user(id: u64, is_self: bool, first: &str, last: &str) -> User {
    let mut u = User::new(UserId(id), is_self);
    u.set_name(first, last, "", "");
    u.lastseen = LastseenStatus::online_till_status(T_307 - 3_600, false, false);
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

fn landscape_thumb() -> Vec<u8> {
    let (w, h) = (40_u32, 26_u32);
    let mut img = RgbaImage::new(w, h);
    for (x, y, px) in img.enumerate_pixels_mut() {
        let left = y as f32 > 13.0 + (x as f32 - 10.0).abs() * 0.9;
        let right = y as f32 > 10.0 + (x as f32 - 28.0).abs() * 0.85;
        let sun = ((x as f32 - 31.0).powi(2) + (y as f32 - 6.0).powi(2)).sqrt() < 3.5;
        let c = if y > 20 {
            [0x1E, 0x3A, 0x5C]
        } else if right {
            [0x26, 0x45, 0x68]
        } else if left {
            [0x34, 0x5E, 0x80]
        } else if sun {
            [0xFF, 0xDB, 0x70]
        } else {
            [0xA0, 0xB0, 0xE0]
        };
        *px = Rgba([c[0], c[1], c[2], 0xFF]);
    }
    tg_media::image::inline::encode_stripped_thumb(&img)
}

fn landscape_photo() -> MessageMedia {
    MessageMedia::Photo(Box::new(PhotoData {
        id: tg_data::PhotoId(880_001),
        sizes: vec![PhotoSize {
            width: 800,
            height: 520,
            ..Default::default()
        }],
        stripped_thumb: Some(landscape_thumb()),
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

/// The five chats of the stage-3 test account (`Test2name Test2surname` is not a contact).
fn parity_session() -> Session {
    let mut s = Session::new(UserId(SELF_ID));
    let me = PeerId::from_user(UserId(SELF_ID));
    s.upsert_user(
        user(SELF_ID, true, "Testname", "Testsurname"),
        PeerUpdateFlags::all(),
    );
    s.upsert_user(
        user(CONTACT_ID, false, "Test2name", "Test2surname"),
        PeerUpdateFlags::all(),
    );
    let mut bot = user(BOT_ID, false, "Suggested Attach Bot", "");
    bot.bot_info = Some(Default::default());
    s.upsert_user(bot, PeerUpdateFlags::all());

    let mut chat = Chat::new(ChatId(GROUP_ID));
    chat.set_name("Parity Test Group");
    chat.creator = UserId(SELF_ID);
    chat.count = 2;
    s.upsert_chat(chat, PeerUpdateFlags::all());

    let mut ch = Channel::new(ChannelId(CHANNEL_ID));
    ch.flags.remove(ChannelFlags::FORBIDDEN);
    ch.set_name("Parity Test Channel", "");
    ch.flags
        .insert(ChannelFlags::BROADCAST | ChannelFlags::CREATOR);
    s.upsert_channel(ch, PeerUpdateFlags::all());

    let contact = PeerId::from_user(UserId(CONTACT_ID));
    s.insert_message(msg(
        contact,
        1,
        Some(contact),
        T_307,
        "Hello everyone from Account B!",
    ));

    let group = PeerId::from_chat(ChatId(GROUP_ID));
    let mut welcome = msg(
        group,
        2,
        Some(me),
        T_307 - 60,
        "Welcome to Parity Test Group! Checking group chat layout and member list.",
    );
    welcome.flags.insert(MessageFlags::OUTGOING);
    s.insert_message(welcome);
    if let Some(h) = s.history_mut(group) {
        h.outbox_read(MsgId(2));
    }

    // A broadcast post nobody "read": tdesktop still draws two ticks (`HistoryItem::unread`).
    let channel = PeerId::from_channel(ChannelId(CHANNEL_ID));
    let mut post = with_media(
        msg(channel, 3, None, T_307 - 13 * 60, ""),
        "Channel photo post",
        landscape_photo(),
    );
    post.flags
        .insert(MessageFlags::POST | MessageFlags::OUTGOING);
    s.insert_message(post);

    s.insert_message(with_media(
        msg(me, 5, Some(me), T_307 - 13 * 60, ""),
        "",
        notes_file(),
    ));

    let bot_peer = PeerId::from_user(UserId(BOT_ID));
    let mut allowed = msg(bot_peer, 6, Some(me), T_307 - 38_280, "");
    allowed.flags.insert(MessageFlags::OUTGOING);
    allowed.content = MessageContent::Service(Box::new(ServiceAction::BotAllowed {
        attach_menu: true,
        from_request: false,
        domain: None,
        app_id: None,
    }));
    s.insert_message(allowed);
    s
}

fn row_peers() -> [PeerId; 5] {
    [
        PeerId::from_user(UserId(CONTACT_ID)),
        PeerId::from_chat(ChatId(GROUP_ID)),
        PeerId::from_channel(ChannelId(CHANNEL_ID)),
        PeerId::from_user(UserId(SELF_ID)),
        PeerId::from_user(UserId(BOT_ID)),
    ]
}

fn dialog_rows(s: &Session) -> Vec<DialogItem> {
    row_peers()
        .into_iter()
        .filter_map(|pid| {
            tg_app_live::convert::dialog_item_from_session(
                s,
                pid,
                DialogListQuery::Filter(FilterId::ALL),
                T_307 + 720,
            )
        })
        .collect()
}

/// The stage-3 "Parity" search: two chats and three messages.
fn parity_search(rows: &[DialogItem]) -> SearchResults {
    let at = |h: u8, m: u8| LocalDateTime::new(2026, 11, 25, h, m, 0);
    let result = |peer: &DialogItem, id: i32, sender: Option<&str>, text: &str, time| {
        MessageSearchResult {
            peer_id: peer.peer_id,
            kind: peer.kind,
            special: peer.special,
            peer_title: peer.title.clone(),
            color_index: peer.color_index,
            msg_id: tg_app::stand_in::types::MsgId(id),
            sender_name: sender.map(str::to_owned),
            text: text.to_owned(),
            text_spoilers: Vec::new(),
            time,
            send_state: TICKS[usize::from(peer.special != SpecialRowKind::SavedMessages)], // after-only
            archived: false,
            media_preview: None,
        }
    };
    SearchResults {
        hashtags: Vec::new(),
        local_chats: vec![rows[1].clone(), rows[2].clone()],
        global_peers: Vec::new(),
        messages: vec![
            result(
                &rows[0],
                11,
                None,
                "Hello! Checking 1:1 chat parity with text, reply, photo, file, and reactions.",
                at(3, 6),
            ),
            result(
                &rows[1],
                12,
                Some("You"),
                "Welcome to Parity Test Group! Checking group chat layout and member list.",
                at(3, 6),
            ),
            result(
                &rows[3],
                13,
                None,
                "Hello! Testing UI parity between Official Telegram Desktop and Rustgram.",
                at(2, 53),
            ),
        ],
        total_messages: 3,
        has_more_messages: false,
        posts_flood: None,
    }
}

/// Saved Messages results draw no ticks; the other stage-3 results were read (two ticks).
const TICKS: [DialogSendState; 2] = [DialogSendState::None, DialogSendState::Received];

fn snap_dir() -> PathBuf {
    std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("uip4_f"))
}

fn save(img: &RgbaImage, path: &Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create snap dir");
    }
    img.save(path).expect("save png");
}

fn save_harness<S>(h: &mut Harness<'static, S>, path: &Path) {
    let img = h.render().expect("wgpu render");
    save(&img, path);
}

fn harness_builder<S>(size: Vec2) -> egui_kittest::HarnessBuilder<S> {
    init_headless_env();
    egui_kittest::HarnessBuilder::<S>::default()
        .with_size(size)
        .with_render_cursor(false)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .renderer(crate::common::serialized_wgpu_renderer())
        .with_context_setup(|ctx| {
            tg_ui::fonts::register_tdesktop_fonts(ctx);
            apply_deterministic_style(ctx);
        })
}

fn capture_rows(dir: &Path, stage: &str, rows: &[DialogItem]) {
    for (mode, mode_label) in [(ThemeMode::Day, "day"), (ThemeMode::Night, "night")] {
        let wide = if mode == ThemeMode::Day {
            OFFICIAL_COLUMN_W
        } else {
            OFFICIAL_COLUMN_W_WIDE
        };
        for (w_label, width) in [("1440x900", wide), ("390x844", 390.0)] {
            let rows_for_ui = rows.to_vec();
            let size = Vec2::new(width, ROW_H * rows.len() as f32);
            let mut h = harness_builder(size).build_ui(move |ui| {
                let palette = Palette::for_mode(mode);
                let colors = DialogColors::new(&palette);
                let painter = ui.painter().clone();
                painter.rect_filled(ui.max_rect(), 0.0, palette.window_bg);
                for (i, item) in rows_for_ui.iter().enumerate() {
                    let rect = Rect::from_min_size(
                        Pos2::new(0.0, i as f32 * ROW_H),
                        Vec2::new(width, ROW_H),
                    );
                    let _ = paint_dialog_row(
                        &painter,
                        rect,
                        item,
                        now_local(),
                        RowPaintState::default(),
                        &palette,
                        &colors,
                    );
                }
            });
            h.run_steps(3);
            save_harness(
                &mut h,
                &dir.join(format!("chat_list_rows_{mode_label}_{w_label}_{stage}.png")),
            );
        }
    }
}

fn capture_search(dir: &Path, stage: &str, rows: &[DialogItem]) {
    let results = parity_search(rows);
    for (w_label, width) in [("1440x900", OFFICIAL_COLUMN_W_WIDE), ("390x844", 390.0)] {
        let results = results.clone();
        let size = Vec2::new(width, 2.0 * ROW_H + 28.0 + 3.0 * ROW_H);
        let state = (DialogsSearchState::default(), MockDialogsStore::new());
        let mut h = harness_builder(size).build_ui_state(
            move |ui, (search, backend): &mut (DialogsSearchState, MockDialogsStore)| {
                let palette = Palette::for_mode(ThemeMode::Day);
                // The whole window is the dialogs column (no panel margin).
                let full = ui.ctx().content_rect();
                let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(full));
                ui.painter().rect_filled(full, 0.0, palette.window_bg);
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                let backend: &dyn DialogsBackend = backend;
                let _ = show_search_body(
                    &mut ui,
                    "Parity",
                    search,
                    &results,
                    backend,
                    AppPeerId(0),
                    now_local(),
                    &palette,
                );
            },
            state,
        );
        h.run_steps(3);
        save_harness(
            &mut h,
            &dir.join(format!("search_results_{w_label}_{stage}.png")),
        );
    }
}

fn open_search(h: &mut Harness<'static, AppTestState>, query: &str) {
    let geo = h.state().controller.session().expect("session").layout();
    let dialogs_right = (geo.filters_width + geo.dialogs_width) as f32;
    let input = h
        .get_all_by_role(egui::accesskit::Role::TextInput)
        .find(|n| n.rect().center().x < dialogs_right && n.rect().center().y < DIALOGS_TOP_BAR_H)
        .expect("dialogs search input");
    input.click();
    h.step();
    type_text(h, query);
    wait_until(h, DEFAULT_MAX_FRAMES, "search mode is active", |h| {
        let dialogs = h.state_mut().controller.main_widget_mut().dialogs_mut();
        dialogs.search_active && dialogs.search_query == query
    });
    step_frames(h, 20);
}

/// Right-clicks the first non-contact user row and hovers "Mute notifications".
fn open_mute_submenu(h: &mut Harness<'static, AppTestState>) -> bool {
    let slice = h.state().controller.mock_session().dialog_slice(
        DialogListQuery::Filter(FilterId::ALL),
        0,
        20,
    );
    let Some(idx) = slice.entries.iter().position(|d| {
        d.kind == DialogPeerKind::User && d.special == SpecialRowKind::None && !d.is_contact
    }) else {
        return false;
    };
    let geo = h.state().controller.session().expect("session").layout();
    let pos = Pos2::new(
        geo.filters_width as f32 + 60.0,
        DIALOGS_TOP_BAR_H + (idx as f32 + 1.5) * ROW_H,
    );
    right_click_at(h, pos);
    let mute = tg_lang::tr::lng_context_mute();
    wait_for_menu_item(h, &mute);
    let Some(center) = h.query_by_label(&mute).map(|n| n.rect().center()) else {
        return false;
    };
    hover_at(h, center);
    step_frames(h, 30);
    true
}

#[test]
#[ignore = "captures before/after PNGs under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_uip4_f_snapshots() {
    let _guard = WGPU_UIP4F_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stage = std::env::var("RUSTGRAM_UIP4F_SNAP_STAGE").unwrap_or_else(|_| "after".to_owned());
    let dir = snap_dir();
    let session = parity_session();
    let rows = dialog_rows(&session);
    assert_eq!(rows.len(), 5, "all five parity chats convert");

    capture_rows(&dir, &stage, &rows);
    capture_search(&dir, &stage, &rows);

    for (size_label, size) in [
        ("1440x900", Vec2::new(1440.0, 900.0)),
        ("390x844", Vec2::new(390.0, 844.0)),
    ] {
        let mut h = build_wgpu_harness(AppTestState::main_workspace(), size);
        settle_layout(&mut h);
        step_frames(&mut h, 10);
        save_harness(
            &mut h,
            &dir.join(format!("chat_list_{size_label}_{stage}.png")),
        );
        // At 390 px the mock workspace opens a chat, not the list; the narrow rows and search
        // body are captured above.
        if size.x < 1000.0 {
            continue;
        }
        if open_mute_submenu(&mut h) {
            save_harness(
                &mut h,
                &dir.join(format!("menu_mute_{size_label}_{stage}.png")),
            );
        }

        let mut h = build_wgpu_harness(AppTestState::main_workspace(), size);
        settle_layout(&mut h);
        open_search(&mut h, "layout");
        save_harness(
            &mut h,
            &dir.join(format!("search_global_{size_label}_{stage}.png")),
        );
    }
}
