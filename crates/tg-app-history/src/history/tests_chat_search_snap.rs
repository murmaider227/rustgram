//! In-chat search harness (mock group chat). Writes `chat_search_<theme>.png` into
//! `$CHATSEARCH_SNAP_DIR` when set; otherwise only renders.

use egui::Vec2;
use egui_kittest::HarnessBuilder;
use egui_kittest::kittest::Queryable;
use tg_text::TextWithEntities;
use tg_ui::fonts::register_tdesktop_fonts;

use crate::history::HistoryWidget;
use crate::history::backend::{HistoryMessage, MessageExtra};
use crate::history::mock::MockHistoryStore;
use tg_app_core::stand_in::strings::EnglishStrings;
use tg_app_core::stand_in::theme::Palette;
use tg_app_core::stand_in::types::{MsgId, PeerId};

pub(crate) struct ChatSearchState {
    pub(crate) widget: HistoryWidget,
    pub(crate) store: MockHistoryStore,
    pub(crate) peer: PeerId,
    palette: Palette,
}

pub(crate) const SEARCH_WORD: &str = "lighthouse";
/// Matches pushed by [`seeded_store`], oldest first.
pub(crate) const MATCH_IDS: [i32; 3] = [90_010, 90_025, 90_038];

pub(crate) fn seeded_store(peer: PeerId) -> MockHistoryStore {
    let mut store = MockHistoryStore::new();
    for i in 0..40 {
        let id = 90_000 + i;
        let text = if MATCH_IDS.contains(&id) {
            format!("Meet at the old {SEARCH_WORD} after the rehearsal, note {i}")
        } else {
            format!("Rehearsal schedule update number {i}: bring the printed notes.")
        };
        store.push_message(
            peer,
            HistoryMessage {
                id: MsgId(id),
                peer_id: peer,
                from_id: PeerId::from_user(1001 + (i as u64 % 3)),
                sender_name: ["Mira Vance", "Theo Park", "Lena Ortiz"][i as usize % 3].to_owned(),
                date_unix: 1_795_340_000 + i * 600,
                outgoing: i % 4 == 0,
                is_post: false,
                text: TextWithEntities::simple(&text),
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
                extra: MessageExtra::default(),
            },
        );
    }
    store
}

pub(crate) fn build_search_harness(
    palette: Palette,
    size: Vec2,
) -> egui_kittest::Harness<'static, ChatSearchState> {
    let peer = PeerId::from_chat(2001);
    let state = ChatSearchState {
        widget: HistoryWidget::new(),
        store: seeded_store(peer),
        peer,
        palette,
    };
    HarnessBuilder::default()
        .with_size(size)
        .with_step_dt(0.1)
        .with_max_steps(30)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .wgpu()
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
        })
        .build_ui_state(
            |ui, st: &mut ChatSearchState| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| {
                        let _ = st.widget.show_with_backend(
                            ui,
                            st.peer,
                            &mut st.store,
                            true,
                            false,
                            false,
                            &st.palette,
                            &EnglishStrings,
                        );
                    });
            },
            state,
        )
}

/// Opens the search, types [`SEARCH_WORD`] into the search field and lets the debounce run.
pub(crate) fn open_and_type(harness: &mut egui_kittest::Harness<'static, ChatSearchState>) {
    for _ in 0..3 {
        harness.step();
    }
    harness.state_mut().widget.open_chat_search();
    for _ in 0..3 {
        harness.step();
    }
    // The search field is the topmost text input (the composer may sit below it).
    harness
        .query_all_by_role(egui::accesskit::Role::TextInput)
        .min_by(|a, b| a.rect().top().total_cmp(&b.rect().top()))
        .expect("search field")
        .type_text(SEARCH_WORD);
    for _ in 0..14 {
        harness.step();
    }
}

#[test]
fn chat_search_snapshot() {
    for (tag, palette) in [("day", Palette::day()), ("night", Palette::night())] {
        let mut harness = build_search_harness(palette, Vec2::new(900.0, 640.0));
        open_and_type(&mut harness);
        if let Some(dir) = std::env::var_os("CHATSEARCH_SNAP_DIR") {
            let img = harness.render().expect("render chat search");
            let path = std::path::PathBuf::from(dir).join(format!("chat_search_{tag}.png"));
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("snap dir");
            }
            img.save(&path).expect("save chat search snapshot");
        }
    }
}
