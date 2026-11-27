//! UI parity stage 2, Package C (chat history, bubbles, composer): before/after captures of the
//! chat screens compared against Telegram Desktop 7.4.6, plus layout assertions.
//!
//! Scenes mirror the parity capture set: 1:1 chat, composer with text, message context menu,
//! group, channel (owner), Saved Messages and the Night 1:1 chat, at 1440x900 and 390x844.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use egui::Vec2;
use tg_app::history::backend::{
    DocumentAttachment, HistoryBackend, HistoryMedia, HistoryMessage, MessageExtra,
    PhotoAttachment, ReactionCounter, ReplyHeader, ServiceAction,
};
use tg_app::info::InfoBackend;
use tg_app::info::backend::CreateChannelRequest;
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::types::{MsgId, PeerId};
use tg_app::window::section::SectionShow;
use tg_text::TextWithEntities;

use crate::common::{AppTestState, build_wgpu_harness, right_click_at, settle_layout, step_frames};

static WGPU_UIP2_C_LOCK: Mutex<()> = Mutex::new(());

/// 2026-11-25 10:00:00 UTC.
const DAY: i32 = 1_795_600_800;

fn save_png(harness: &mut egui_kittest::Harness<'static, AppTestState>, path: &Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create snap dir");
    }
    let img = harness.render().expect("wgpu render");
    img.save(path).expect("save png");
}

/// Procedural landscape (sky, sun on the right, two mountains) encoded as stripped bytes.
fn landscape_stripped() -> Vec<u8> {
    let img = image::RgbaImage::from_fn(40, 26, |x, y| {
        let (xf, yf) = (x as f32, y as f32);
        let sun = ((xf - 31.0).powi(2) + (yf - 6.0).powi(2)).sqrt() < 4.0;
        let ridge = (12.0 + (xf - 10.0).abs() * 0.9).min(8.0 + (xf - 28.0).abs() * 0.8);
        if yf >= 21.0 {
            image::Rgba([30, 45, 70, 255])
        } else if yf >= ridge {
            image::Rgba([45, 75, 110, 255])
        } else if sun {
            image::Rgba([250, 215, 110, 255])
        } else {
            let t = yf / 21.0;
            image::Rgba([
                (225.0 - 140.0 * t) as u8,
                (215.0 - 70.0 * t) as u8,
                (225.0 + 20.0 * t) as u8,
                255,
            ])
        }
    });
    tg_media::image::inline::encode_stripped_thumb(&img)
}

#[allow(clippy::too_many_arguments)]
fn message(
    id: i32,
    peer: PeerId,
    from: PeerId,
    name: &str,
    date: i32,
    outgoing: bool,
    text: &str,
    media: Option<HistoryMedia>,
) -> HistoryMessage {
    HistoryMessage {
        id: MsgId(id),
        peer_id: peer,
        from_id: from,
        sender_name: name.to_owned(),
        date_unix: date,
        outgoing,
        is_post: false,
        text: TextWithEntities::simple(text),
        media,
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

fn photo(caption_seed: u8) -> HistoryMedia {
    HistoryMedia::Photo(PhotoAttachment {
        width: 1280,
        height: 832,
        stripped_thumb: Some(landscape_stripped()),
        fallback_rgb: ([120, 150, 120], [80, 110, 90 + caption_seed]),
        file_name: "landscape.jpg".to_owned(),
        size_bytes: 120_000,
    })
}

fn document() -> HistoryMedia {
    HistoryMedia::Document(DocumentAttachment {
        file_name: "sample_notes.txt".to_owned(),
        size_bytes: 88,
        mime_type: "text/plain".to_owned(),
        thumb_rgb: None,
    })
}

fn clear_history(state: &mut AppTestState, peer: PeerId) {
    let mut handle = state.controller.session_handle();
    let ids: Vec<MsgId> = handle
        .load_slice_around(peer, MsgId::SHOW_AT_END, 1000, 0)
        .messages
        .iter()
        .map(|m| m.id)
        .collect();
    if !ids.is_empty() {
        let _ = HistoryBackend::delete_messages(&mut handle, peer, &ids, true);
    }
}

/// Mirrors the parity 1:1 scene: outgoing text, incoming with a reaction, outgoing reply,
/// outgoing photo with caption, outgoing file, incoming text.
fn seed_one_to_one(state: &mut AppTestState) -> (PeerId, MsgId) {
    let peer = session_peers::USER_MIRA;
    let me = session_peers::SELF;
    clear_history(state, peer);
    let handle = state.controller.session_handle();
    handle.push_history_message(
        peer,
        message(
            9701,
            peer,
            me,
            "Taylor Kim",
            DAY,
            true,
            "Hello! Checking 1:1 chat parity with text, reply, photo, file, and reactions.",
            None,
        ),
    );
    let mut incoming = message(
        9702,
        peer,
        peer,
        "Mira Lane",
        DAY + 20,
        false,
        "Hi! Testing incoming message bubble and reply styling.",
        None,
    );
    incoming.reactions = vec![ReactionCounter {
        emoji: "👍".to_owned(),
        count: 1,
        chosen: true,
    }];
    handle.push_history_message(peer, incoming);
    let mut reply = message(
        9703,
        peer,
        me,
        "Taylor Kim",
        DAY + 60,
        true,
        "Replying to your message to compare reply quotes and accents!",
        None,
    );
    reply.reply = Some(ReplyHeader {
        reply_to_msg_id: MsgId(9702),
        author_name: "Mira Lane".to_owned(),
        author_color_id: peer.bare(),
        author_accent: None,
        preview_text: "Hi! Testing incoming message bubble and reply styling.".to_owned(),
        preview_thumb_rgb: None,
        preview_thumb: None,
        preview_colored_len: 0,
        quote_text: None,
    });
    handle.push_history_message(peer, reply);
    handle.push_history_message(
        peer,
        message(
            9704,
            peer,
            me,
            "Taylor Kim",
            DAY + 70,
            true,
            "Sample landscape photo",
            Some(photo(0)),
        ),
    );
    handle.push_history_message(
        peer,
        message(
            9705,
            peer,
            me,
            "Taylor Kim",
            DAY + 80,
            true,
            "",
            Some(document()),
        ),
    );
    handle.push_history_message(
        peer,
        message(
            9706,
            peer,
            peer,
            "Mira Lane",
            DAY + 120,
            false,
            "Hello everyone!",
            None,
        ),
    );
    (peer, MsgId(9702))
}

fn seed_group(state: &mut AppTestState) -> PeerId {
    let peer = session_peers::GROUP_UI_LAB;
    let me = session_peers::SELF;
    clear_history(state, peer);
    let handle = state.controller.session_handle();
    let mut created = message(9711, peer, me, "Taylor Kim", DAY, true, "", None);
    created.service = Some(ServiceAction::ChatCreated {
        title: "egui & Style System".to_owned(),
    });
    handle.push_history_message(peer, created);
    handle.push_history_message(
        peer,
        message(
            9712,
            peer,
            me,
            "Taylor Kim",
            DAY + 30,
            true,
            "Welcome to the group! Checking group chat layout and member list.",
            None,
        ),
    );
    peer
}

/// A channel the account owns (`can_post_messages`), like the official capture's test channel.
fn seed_channel(state: &mut AppTestState) -> PeerId {
    let mut handle = state.controller.session_handle();
    let peer = handle
        .create_channel(CreateChannelRequest {
            title: "Nightly Builds".to_owned(),
            description: String::new(),
            is_public: false,
            username: None,
            no_forwards: false,
            has_custom_photo: false,
        })
        .unwrap_or(session_peers::CHANNEL_RELEASES);
    clear_history(state, peer);
    let mut created = message(9721, peer, peer, "Nightly Builds", DAY, false, "", None);
    created.is_post = true;
    created.service = Some(ServiceAction::ChatCreated {
        title: "Nightly Builds".to_owned(),
    });
    handle.push_history_message(peer, created);
    let mut post = message(
        9722,
        peer,
        peer,
        "Nightly Builds",
        DAY + 30,
        false,
        "Welcome to the channel! Checking channel layout, views counter, and broadcast bar.",
        None,
    );
    post.is_post = true;
    post.views = Some(1);
    handle.push_history_message(peer, post);
    let mut photo_post = message(
        9723,
        peer,
        peer,
        "Nightly Builds",
        DAY + 60,
        false,
        "Channel photo post",
        Some(photo(1)),
    );
    photo_post.is_post = true;
    photo_post.views = Some(1);
    handle.push_history_message(peer, photo_post);
    peer
}

fn seed_saved(state: &mut AppTestState) -> PeerId {
    let peer = session_peers::SELF;
    clear_history(state, peer);
    let handle = state.controller.session_handle();
    handle.push_history_message(
        peer,
        message(
            9731,
            peer,
            peer,
            "Taylor Kim",
            DAY,
            true,
            "Hello! Testing UI parity between the official client and Rustgram.",
            None,
        ),
    );
    let mut reply = message(
        9732,
        peer,
        peer,
        "Taylor Kim",
        DAY + 30,
        true,
        "This is a reply to check reply quote styling and bar colors.",
        None,
    );
    reply.reply = Some(ReplyHeader {
        reply_to_msg_id: MsgId(9731),
        author_name: "Taylor Kim".to_owned(),
        author_color_id: peer.bare(),
        author_accent: None,
        preview_text: "Hello! Testing UI parity between the official client and Rustgram."
            .to_owned(),
        preview_thumb_rgb: None,
        preview_thumb: None,
        preview_colored_len: 0,
        quote_text: None,
    });
    handle.push_history_message(peer, reply);
    handle.push_history_message(
        peer,
        message(
            9733,
            peer,
            peer,
            "Taylor Kim",
            DAY + 60,
            true,
            "Sample landscape photo",
            Some(photo(2)),
        ),
    );
    handle.push_history_message(
        peer,
        message(
            9734,
            peer,
            peer,
            "Taylor Kim",
            DAY + 90,
            true,
            "",
            Some(document()),
        ),
    );
    peer
}

fn open(h: &mut egui_kittest::Harness<'static, AppTestState>, peer: PeerId) {
    if let Some(session) = h.state_mut().controller.session_mut() {
        session.show_peer_history(peer, SectionShow::clear_stack(), MsgId::SHOW_AT_END);
    }
    settle_layout(h);
    h.state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .inner_mut()
        .request_scroll_to_bottom();
    step_frames(h, 12);
}

fn snap_dir() -> PathBuf {
    std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("uip2_c"))
}

#[test]
#[ignore = "captures before/after parity PNGs under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_uip2_c_snapshots() {
    let _guard = WGPU_UIP2_C_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stage = std::env::var("RUSTGRAM_UIP2_SNAP_STAGE").unwrap_or_else(|_| "after".to_owned());
    let dir = snap_dir();
    // Compare with the official client in the machine's own time zone.
    crate::common::init_headless_env();
    tg_history::datetime::set_utc_offset_override(None);
    for (label, size) in [
        ("1440x900", Vec2::new(1440.0, 900.0)),
        ("390x844", Vec2::new(390.0, 844.0)),
    ] {
        let shot = |h: &mut egui_kittest::Harness<'static, AppTestState>, name: &str| {
            save_png(h, &dir.join(format!("{name}_{label}_{stage}.png")));
        };

        // 02 one-to-one chat, 05 composer with text, 09 context menu.
        {
            let mut state = AppTestState::main_workspace();
            let (peer, incoming) = seed_one_to_one(&mut state);
            let mut h = build_wgpu_harness(state, size);
            settle_layout(&mut h);
            open(&mut h, peer);
            shot(&mut h, "02_one_to_one_chat");

            h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .compose_mut()
                .insert_text("Checking composer focus, caret, and send button styling", 0);
            step_frames(&mut h, 6);
            shot(&mut h, "05_composer_text");

            h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .compose_mut()
                .cancel_mode();
            let bubble = h
                .state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .inner_mut()
                .last_painted_bubble_rect(incoming);
            if let Some(rect) = bubble {
                right_click_at(&mut h, rect.center());
                step_frames(&mut h, 20);
                shot(&mut h, "09_message_context_menu");
            }
        }

        // 03 group chat.
        {
            let mut state = AppTestState::main_workspace();
            let peer = seed_group(&mut state);
            let mut h = build_wgpu_harness(state, size);
            settle_layout(&mut h);
            open(&mut h, peer);
            shot(&mut h, "03_group_chat");
        }

        // 04 channel.
        {
            let mut state = AppTestState::main_workspace();
            let peer = seed_channel(&mut state);
            let mut h = build_wgpu_harness(state, size);
            settle_layout(&mut h);
            open(&mut h, peer);
            shot(&mut h, "04_channel");
        }

        // 23 Saved Messages.
        {
            let mut state = AppTestState::main_workspace();
            let peer = seed_saved(&mut state);
            let mut h = build_wgpu_harness(state, size);
            settle_layout(&mut h);
            open(&mut h, peer);
            shot(&mut h, "23_saved_messages");
        }

        // 25 Night 1:1 chat.
        {
            let mut state = AppTestState::main_workspace();
            let (peer, _) = seed_one_to_one(&mut state);
            state.controller.toggle_night_mode();
            let mut h = build_wgpu_harness(state, size);
            settle_layout(&mut h);
            open(&mut h, peer);
            shot(&mut h, "25_night_chat");
        }
    }
}
