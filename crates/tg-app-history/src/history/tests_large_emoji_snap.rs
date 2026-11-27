//! TGB-3054: messages of only 1-3 emoji are drawn large without a bubble; "Large emoji" off
//! draws them as text again.
//!
//! Writes `large_emoji_<theme>_<scale>_<on|off>.png` into `$LARGE_EMOJI_SNAP_DIR` when set.
//! "off" is the code path every emoji-only message took before TGB-3054.

use std::sync::Arc;

use egui::{Pos2, Rect, Vec2};
use egui_kittest::{Harness, HarnessBuilder};
use tg_text::TextWithEntities;
use tg_ui::fonts::register_tdesktop_fonts;

use crate::history::backend::{HistoryMessage, HistorySlice};
use crate::history::inner::HistoryInnerWidget;
use crate::history::mock::MockHistoryStore;
use crate::history::tests_spoiler_snap::{SnapState, message};
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind, PeerSummary};

fn messages(peer: PeerId) -> Vec<HistoryMessage> {
    let text = |s: &str| TextWithEntities::simple(s);
    let mut list = vec![
        message(peer, 1, 701, "Тестовая Оля", text("Смотри, что нашла")),
        message(peer, 2, 701, "Тестовая Оля", text("\u{1F600}")),
        message(
            peer,
            3,
            701,
            "Тестовая Оля",
            text("\u{1F525}\u{2764}\u{FE0F}\u{1F44D}"),
        ),
        message(peer, 4, 702, "Я", text("\u{1F602}\u{1F602}")),
        message(peer, 5, 702, "Я", text("Ок \u{1F44C}")),
        message(peer, 6, 702, "Я", text("\u{1F3B8}")),
    ];
    for msg in &mut list[3..] {
        msg.outgoing = true;
    }
    list
}

fn peer_info(peer: PeerId) -> crate::history::backend::HistoryPeerInfo {
    crate::history::backend::HistoryPeerInfo {
        summary: PeerSummary {
            id: peer,
            kind: PeerKind::User,
            title: "Snapshot".to_owned(),
            status: String::new(),
            username: None,
            phone_or_link: None,
            about: String::new(),
            initials: "TG".to_owned(),
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

fn build(mode: ThemeMode, ppp: f32, large_emoji: bool) -> Harness<'static, SnapState> {
    let peer = PeerId::from_user(88_054);
    let messages = messages(peer);
    let mut inner = HistoryInnerWidget::new();
    inner.set_large_emoji(large_emoji);
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
        PeerKind::User,
        true,
    );
    let size = Vec2::new(560.0, 520.0);
    let state = SnapState::new(
        inner,
        MockHistoryStore::new(),
        peer_info(peer),
        Rect::from_min_size(Pos2::ZERO, size),
        HistoryColors::from_palette(&Palette::for_mode(mode)),
    );
    let mut harness = HarnessBuilder::default()
        .with_size(size)
        .with_pixels_per_point(ppp)
        .with_step_dt(0.05)
        .with_max_steps(8)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .wgpu()
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
            tg_emoji::install_text_emoji(ctx, Arc::new(|| (18.0, 1.0)));
        })
        .build_ui_state(|ui, st: &mut SnapState| st.show(ui), state);
    for _ in 0..6 {
        harness.step();
    }
    harness
}

fn has_bubble(harness: &Harness<'static, SnapState>, id: i32) -> bool {
    harness
        .state()
        .inner
        .cached_message_layout(MsgId(id))
        .expect("laid out")
        .has_bubble
}

#[test]
fn emoji_only_messages_have_no_bubble_and_the_setting_brings_it_back() {
    let mut harness = build(ThemeMode::Day, 1.0, true);
    for id in [2, 3, 4, 6] {
        assert!(!has_bubble(&harness, id), "message {id} is emoji only");
        let layout = harness
            .state()
            .inner
            .cached_message_layout(MsgId(id))
            .expect("laid out");
        assert!(layout.unwrapped);
        assert!(layout.media_rect.height >= 38);
    }
    for id in [1, 5] {
        assert!(has_bubble(&harness, id), "message {id} has words");
    }
    // Settings › Chat › Large emoji off: every message is a bubble of text again.
    harness.state_mut().inner.set_large_emoji(false);
    for _ in 0..4 {
        harness.step();
    }
    for id in 1..=6 {
        assert!(
            has_bubble(&harness, id),
            "message {id} with large emoji off"
        );
    }
}

#[test]
fn large_emoji_snapshot() {
    let Some(dir) = std::env::var_os("LARGE_EMOJI_SNAP_DIR").map(std::path::PathBuf::from) else {
        let harness = build(ThemeMode::Day, 1.5, true);
        assert!(!has_bubble(&harness, 2));
        return;
    };
    std::fs::create_dir_all(&dir).expect("create snap dir");
    for (theme, mode) in [("day", ThemeMode::Day), ("night", ThemeMode::Night)] {
        for (scale, ppp) in [("100", 1.0), ("150", 1.5)] {
            for (tag, on) in [("on", true), ("off", false)] {
                let mut harness = build(mode, ppp, on);
                let image = harness.render().expect("render");
                image
                    .save(dir.join(format!("large_emoji_{theme}_{scale}_{tag}.png")))
                    .expect("save snapshot");
            }
        }
    }
}
