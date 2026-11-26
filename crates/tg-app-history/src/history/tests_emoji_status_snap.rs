//! TGB-2964 evidence: a group sender's custom emoji status after the name.
//!
//! `emoji_status_snapshot` writes `emoji_status.png` into `$EMOJI_STATUS_SNAP_DIR` when that
//! variable is set. With `$EMOJI_STATUS_SNAP_BEFORE` set the status document is not registered,
//! as live left it before the loader (the premium star is painted in its place). Mock data only;
//! the status is a synthetic heart bitmap.

use egui::{Pos2, Rect, Vec2};
use egui_kittest::HarnessBuilder;
use tg_text::TextWithEntities;
use tg_ui::custom_emoji::{CustomEmojiAsset, register_custom_emoji_asset};
use tg_ui::fonts::register_tdesktop_fonts;

use crate::history::backend::{
    HistoryMessage, HistoryPeerInfo, HistorySlice, MessageExtra, SenderNameStatus,
};
use crate::history::inner::{HistoryInnerWidget, InnerEvent};
use crate::history::mock::MockHistoryStore;
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind, PeerSummary};

const BASE_UNIX: i32 = 1_795_340_000;
const STATUS_ID: u64 = 9_300_001;

fn peer() -> PeerId {
    PeerId::from_channel(77_964)
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

fn group_info() -> HistoryPeerInfo {
    HistoryPeerInfo {
        summary: PeerSummary {
            id: peer(),
            kind: PeerKind::Group,
            title: "Studio Crew".to_owned(),
            status: "4 members".to_owned(),
            username: None,
            phone_or_link: None,
            about: String::new(),
            initials: "SC".to_owned(),
            avatar_rgb: [0x5E, 0x72, 0xE4],
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

fn message(id: i32, from: u64, name: &str, text: &str, status: Option<u64>) -> HistoryMessage {
    HistoryMessage {
        id: MsgId(id),
        peer_id: peer(),
        from_id: PeerId::from_user(from),
        sender_name: name.to_owned(),
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
        extra: MessageExtra {
            sender_status: Some(SenderNameStatus {
                emoji_status_id: status,
                premium: true,
            }),
            ..MessageExtra::default()
        },
    }
}

struct SnapState {
    inner: HistoryInnerWidget,
    store: MockHistoryStore,
    peer_info: HistoryPeerInfo,
    list_rect: Rect,
    colors: HistoryColors,
    events: Vec<InnerEvent>,
}

#[test]
fn emoji_status_snapshot() {
    if std::env::var_os("EMOJI_STATUS_SNAP_BEFORE").is_none() {
        assert!(register_custom_emoji_asset(
            STATUS_ID.to_string(),
            &heart_asset()
        ));
    }
    let size = Vec2::new(520.0, 200.0);
    let messages = vec![
        message(
            1,
            1101,
            "Rin Vale",
            "the auto cleanup runs at night now",
            Some(STATUS_ID),
        ),
        message(2, 1102, "Oda Finch", "nice, and the bot?", None),
        message(
            3,
            1101,
            "Rin Vale",
            "same user bot as before",
            Some(STATUS_ID),
        ),
    ];
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
        PeerKind::Group,
        true,
    );
    let state = SnapState {
        inner,
        store: MockHistoryStore::new(),
        peer_info: group_info(),
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
    if let Some(dir) = std::env::var_os("EMOJI_STATUS_SNAP_DIR") {
        let img = harness.render().expect("render emoji status snapshot");
        let path = std::path::PathBuf::from(dir).join("emoji_status.png");
        img.save(&path).expect("save emoji status snapshot");
    }
}
