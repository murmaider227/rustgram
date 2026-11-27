//! TGB-3124: in a group, tdesktop draws the userpic next to own messages too when the chat is
//! wide (`Message::hasFromPhoto`, `ElementChatMode::Wide`: every message on the left), and not
//! when it is narrow (own messages on the right). Mock data only.
//!
//! Writes `wide_userpics_<theme>_<narrow|wide>.png` into `$WIDE_USERPICS_SNAP_DIR` when set.

use egui::{Pos2, Rect, Vec2};
use egui_kittest::{Harness, HarnessBuilder};
use tg_ui::fonts::register_tdesktop_fonts;

use crate::history::backend::{HistoryMessage, HistoryPeerInfo, HistorySlice};
use crate::history::inner::{HistoryInnerWidget, InnerEvent};
use crate::history::mock::MockHistoryStore;
use crate::history::tests_w4_text_snap::{group_info, group_peer, message};
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::{MsgId, PeerKind};

struct WideState {
    inner: HistoryInnerWidget,
    store: MockHistoryStore,
    peer_info: HistoryPeerInfo,
    list_rect: Rect,
    colors: HistoryColors,
    wide: bool,
    events: Vec<InnerEvent>,
}

const OWN: i32 = 2;

fn messages() -> Vec<HistoryMessage> {
    let peer = group_peer();
    let mut own = message(peer, OWN, 1001, "Mira Vance", "On my way, ten minutes");
    own.outgoing = true;
    vec![
        message(peer, 1, 1002, "Ivo Marten", "Where is everyone?"),
        own,
        message(peer, 3, 1002, "Ivo Marten", "Great, see you"),
    ]
}

fn build(mode: ThemeMode, wide: bool) -> Harness<'static, WideState> {
    let peer = group_peer();
    let mut inner = HistoryInnerWidget::new();
    let messages = messages();
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
        PeerKind::Group,
        true,
    );
    let size = Vec2::new(900.0, 320.0);
    let state = WideState {
        inner,
        store: MockHistoryStore::new(),
        peer_info: group_info(peer),
        list_rect: Rect::from_min_size(Pos2::ZERO, size),
        colors: HistoryColors::from_palette(&Palette::for_mode(mode)),
        wide,
        events: Vec::new(),
    };
    let mut harness = HarnessBuilder::default()
        .with_size(size)
        .with_step_dt(0.05)
        .with_max_steps(8)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .wgpu()
        .with_context_setup(register_tdesktop_fonts)
        .build_ui_state(
            |ui, st: &mut WideState| {
                ui.painter()
                    .rect_filled(st.list_rect, 0.0, st.colors.canvas_bg);
                let evs = st.inner.show(
                    ui,
                    st.list_rect,
                    &st.peer_info,
                    st.wide,
                    "",
                    &st.colors,
                    &mut st.store,
                );
                st.events.extend(evs);
            },
            state,
        );
    harness.run_steps(4);
    harness
}

fn own_has_userpic(harness: &Harness<'static, WideState>) -> bool {
    let inner = &harness.state().inner;
    let index = inner
        .messages()
        .iter()
        .position(|m| m.id == MsgId(OWN))
        .expect("own message");
    inner.list_items()[index].has_from_photo
}

#[test]
fn own_group_messages_get_a_userpic_only_in_the_wide_layout() {
    let mut harness = build(ThemeMode::Day, true);
    assert!(
        own_has_userpic(&harness),
        "wide: every message has its userpic"
    );
    // The window narrows: own messages go right, without a userpic.
    harness.state_mut().wide = false;
    harness.run_steps(3);
    assert!(!own_has_userpic(&harness));
    let narrow = build(ThemeMode::Day, false);
    assert!(!own_has_userpic(&narrow));
}

#[test]
fn wide_userpics_snapshot() {
    let Some(dir) = std::env::var_os("WIDE_USERPICS_SNAP_DIR").map(std::path::PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&dir).expect("create snap dir");
    for (theme, mode) in [("day", ThemeMode::Day), ("night", ThemeMode::Night)] {
        for (tag, wide) in [("narrow", false), ("wide", true)] {
            let mut harness = build(mode, wide);
            let image = harness.render().expect("render");
            image
                .save(dir.join(format!("wide_userpics_{theme}_{tag}.png")))
                .expect("save snapshot");
        }
    }
}
