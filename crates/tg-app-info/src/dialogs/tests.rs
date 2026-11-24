//! Unit tests for dialogs backend, virtualization, drag-and-drop reorder, search, shortcuts,
//! and an ignored screenshot harness producing `crates/tg-app/docs/dialogs.png`.

use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use egui::{CornerRadius, Pos2, Rect, Stroke, UiBuilder, Vec2};
use tg_history::DialogSendState;

use crate::dialogs::backend::{
    ChatTypeFilter, DialogListQuery, DialogPeerKind, DialogsBackend, FolderUnreadState,
    SearchRequest, SearchTab, SpecialRowKind,
};
use crate::dialogs::filters_menu::{FILTERS_SIDEBAR_WIDTH, format_sidebar_badge};
use crate::dialogs::inner::{PinnedDragState, variable_visible_row_range, visible_row_range};
use crate::dialogs::layout::find_highlight_ranges;
use crate::dialogs::mock::{
    FOLDER_CHANNELS, FOLDER_PERSONAL, FOLDER_UNREAD, FOLDER_WORK, MockDialogsStore, PEER_REPLIES,
    PEER_SAVED_MESSAGES, reset_mock_backend_dialogs_state,
};
use crate::dialogs::search::{
    HashOrCashtag, fix_hashtag_search_query, is_hash_or_cashtag_search_query,
};
use crate::dialogs::widget::{
    DialogsPanel, DialogsPanelAction, DialogsShortcut, resolve_folder_slot,
};
use tg_app_core::stand_in::strings::EnglishStrings;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::{FilterId, PeerId};

#[test]
fn mock_dialogs_pagination_folders_and_special_rows() {
    let store = MockDialogsStore::new();
    let page1 = store.dialog_slice(DialogListQuery::Filter(FilterId::ALL), 0, 25);
    assert_eq!(page1.entries.len(), 25);
    assert!(page1.has_more);
    assert!(page1.total >= 250);
    assert_eq!(page1.pinned_count, 5);
    assert_eq!(page1.entries[0].peer_id, PEER_SAVED_MESSAGES);
    assert_eq!(page1.entries[0].special, SpecialRowKind::SavedMessages);
    assert_eq!(page1.entries[0].send_state, DialogSendState::Received);

    // Replies special row exists.
    let replies = store
        .dialog_by_peer(PEER_REPLIES)
        .expect("Replies special row");
    assert_eq!(replies.special, SpecialRowKind::Replies);

    // Second page starts right after page1 without overlap.
    let page2 = store.dialog_slice(DialogListQuery::Filter(FilterId::ALL), 25, 25);
    assert_eq!(page2.entries.len(), 25);
    assert_ne!(page1.entries[24].peer_id, page2.entries[0].peer_id);

    // Folder filtering works for Work, Channels, and Unread.
    let channels = store.dialog_slice(DialogListQuery::Filter(FOLDER_CHANNELS), 0, 100);
    assert!(!channels.entries.is_empty());
    assert!(
        channels
            .entries
            .iter()
            .all(|d| d.folders.contains(&FOLDER_CHANNELS))
    );

    let unread = store.dialog_slice(DialogListQuery::Filter(FOLDER_UNREAD), 0, 100);
    assert!(!unread.entries.is_empty());
    assert!(unread.entries.iter().all(|d| d.badges.unread));
}

#[test]
fn mock_backend_trait_impl_mutations_and_archive() {
    let mut backend = super::mock::MockDialogsBackendAdapter::new(true);
    reset_mock_backend_dialogs_state(&backend);

    // Archive summary starts expanded with 24 archived chats.
    let summary = backend.archive_summary().expect("archive populated");
    assert!(summary.chat_count >= 20);
    assert!(!summary.collapsed);

    backend.set_archive_collapsed(true);
    assert!(backend.archive_collapsed());
    assert!(backend.archive_summary().expect("summary").collapsed);

    // Mark archive read zeroes unread count.
    backend.mark_archive_read();
    assert_eq!(backend.archive_summary().expect("summary").unread_count, 0);

    // Toggle pin, mute, read/unread, archive, clear, delete on a non-special dialog.
    let slice = backend.dialog_slice(DialogListQuery::Filter(FilterId::ALL), 0, 20);
    let target = slice
        .entries
        .iter()
        .find(|d| !d.pinned && d.special == SpecialRowKind::None)
        .expect("unpinned chat")
        .peer_id;

    assert!(backend.toggle_pin(target, DialogListQuery::Filter(FilterId::ALL)));
    let after_pin = backend.dialog_slice(DialogListQuery::Filter(FilterId::ALL), 0, 10);
    assert_eq!(after_pin.entries[0].peer_id, target);
    assert!(!backend.toggle_pin(target, DialogListQuery::Filter(FilterId::ALL)));

    let was_muted = backend.dialog_by_peer(target).expect("exists").muted;
    assert_eq!(backend.toggle_mute_dialog(target), !was_muted);

    backend.mark_dialog_unread(target);
    assert!(
        backend
            .dialog_by_peer(target)
            .expect("exists")
            .badges
            .unread
    );
    backend.mark_dialog_read(target);
    assert!(
        !backend
            .dialog_by_peer(target)
            .expect("exists")
            .badges
            .unread
    );

    assert!(backend.toggle_archive(target));
    assert!(backend.dialog_by_peer(target).expect("exists").archived);
    assert!(!backend.toggle_archive(target));

    backend.clear_history(target);
    assert!(
        backend
            .dialog_by_peer(target)
            .expect("exists")
            .last_message
            .is_empty()
    );

    backend.delete_dialog(target);
    assert!(backend.dialog_by_peer(target).is_none());

    reset_mock_backend_dialogs_state(&backend);
}

#[test]
fn pinned_reorder_drag_math_and_backend_commit() {
    let mut store = MockDialogsStore::new();
    let list = DialogListQuery::Filter(FilterId::ALL);
    let before = store.dialog_slice(list, 0, 5);
    assert_eq!(before.pinned_count, 5);
    let p0 = before.entries[0].peer_id;
    let p1 = before.entries[1].peer_id;
    let p2 = before.entries[2].peer_id;

    // Simulate dragging row 0 downward past row 1 and row 2 (each 62px).
    let heights = [62.0_f32; 5];
    let mut drag = PinnedDragState::start(p0, 0, 31.0, 62.0);
    // Small move below half-height threshold (31px) -> no swap yet.
    assert!(drag.update(55.0, &heights).is_empty());
    assert!(drag.moved);

    // Move across 2 full rows (+124px from 31.0 -> 155.0) -> swaps (0->1) and (1->2).
    let swaps = drag.update(155.0, &heights);
    assert_eq!(swaps, vec![(0, 1), (1, 2)]);
    assert_eq!(drag.dragging_index, 2);

    for (from, to) in swaps {
        assert!(store.reorder_pinned(list, from, to));
    }

    let after = store.dialog_slice(list, 0, 5);
    assert_eq!(after.entries[0].peer_id, p1);
    assert_eq!(after.entries[1].peer_id, p2);
    assert_eq!(after.entries[2].peer_id, p0);

    // Dragging back up from index 2 to 0.
    let up_swaps = drag.update(31.0, &heights);
    assert_eq!(up_swaps, vec![(2, 1), (1, 0)]);
    assert_eq!(drag.dragging_index, 0);
}

#[test]
fn search_local_global_messages_hashtags_and_filters() {
    let mut store = MockDialogsStore::new();

    // Text query hits local chats, global peers, and messages.
    let res = store.search_dialogs(&SearchRequest {
        query: "rust".to_owned(),
        tab: SearchTab::Chats,
        chat_type_filter: ChatTypeFilter::All,
        ..Default::default()
    });
    assert!(!res.local_chats.is_empty());
    assert!(!res.global_peers.is_empty());
    assert!(!res.messages.is_empty());
    assert!(res.hashtags.is_empty());

    // Channels tab restricts results to channels.
    let ch_res = store.search_dialogs(&SearchRequest {
        query: "rust".to_owned(),
        tab: SearchTab::Channels,
        ..Default::default()
    });
    assert!(
        ch_res
            .local_chats
            .iter()
            .all(|c| c.kind == DialogPeerKind::Channel)
    );
    assert!(
        ch_res
            .global_peers
            .iter()
            .all(|g| g.kind == DialogPeerKind::Channel)
    );

    // Hashtag prefix returns completions and matching messages.
    let tag_res = store.search_dialogs(&SearchRequest {
        query: "#re".to_owned(),
        ..Default::default()
    });
    assert!(tag_res.hashtags.iter().any(|h| h.tag == "release"));
    assert!(tag_res.local_chats.is_empty());

    // Recent peers and top peers mutations.
    let initial_recent = store.recent_search_peers().len();
    assert!(initial_recent >= 3);
    let first_recent = store.recent_search_peers()[0].peer_id;
    store.remove_recent_search_peer(first_recent);
    assert_eq!(store.recent_search_peers().len(), initial_recent - 1);
    store.add_recent_search_peer(first_recent);
    assert_eq!(store.recent_search_peers()[0].peer_id, first_recent);
    store.clear_recent_search_peers();
    assert!(store.recent_search_peers().is_empty());
}

#[test]
fn hashtag_query_normalization_and_highlight_spans() {
    assert_eq!(
        is_hash_or_cashtag_search_query("#rust"),
        HashOrCashtag::Hashtag
    );
    assert_eq!(
        is_hash_or_cashtag_search_query("$TON"),
        HashOrCashtag::Cashtag
    );
    assert_eq!(
        is_hash_or_cashtag_search_query("#two words"),
        HashOrCashtag::None
    );
    assert_eq!(
        is_hash_or_cashtag_search_query("plain"),
        HashOrCashtag::None
    );

    assert_eq!(
        fix_hashtag_search_query("  #ru st ", HashOrCashtag::Hashtag),
        "#rust"
    );
    assert_eq!(
        fix_hashtag_search_query("ton", HashOrCashtag::Cashtag),
        "$ton"
    );

    // Highlight range matching is case-insensitive and non-overlapping.
    let text = "Rust port of dialogs_layout in RUST";
    let ranges = find_highlight_ranges(text, "rust");
    assert_eq!(ranges.len(), 2);
    assert_eq!(&text[ranges[0].clone()], "Rust");
    assert_eq!(&text[ranges[1].clone()], "RUST");
    assert!(find_highlight_ranges(text, "").is_empty());
}

#[test]
fn virtualized_range_scales_to_thousands_of_rows() {
    let store = MockDialogsStore::with_total_dialogs(5_000);
    let slice = store.dialog_slice(DialogListQuery::Filter(FilterId::ALL), 0, 5_000);
    assert!(slice.total >= 4_900);

    // Uniform 62px viewport at scroll y = 62_000..62_620 (~10 visible rows).
    let range = visible_row_range(62_000.0, 62_620.0, 62.0, slice.total);
    assert_eq!(range, 1000..1010);

    // Variable-height mix (alternating 62px and 72px = 134px per pair).
    let heights: Vec<f32> = (0..4_000)
        .map(|i| if i % 2 == 0 { 62.0 } else { 72.0 })
        .collect();
    let (vrange, start_y) = variable_visible_row_range(13_400.0, 14_000.0, &heights);
    assert_eq!(vrange.start, 200);
    assert!((start_y - 13_400.0).abs() < 1e-3);
    assert!(vrange.len() <= 12);
}

#[test]
fn keyboard_navigation_and_folder_shortcuts() {
    let mut store = MockDialogsStore::new();
    let mut panel = DialogsPanel::new();
    let folders = store.chat_folders();

    // Ctrl+1..9 folder slot mapping (`Ctrl+9` selects last folder).
    assert_eq!(resolve_folder_slot(&folders, 0), Some(FilterId::ALL));
    assert_eq!(resolve_folder_slot(&folders, 1), Some(FOLDER_PERSONAL));
    assert_eq!(resolve_folder_slot(&folders, 2), Some(FOLDER_WORK));
    assert_eq!(
        resolve_folder_slot(&folders, 8),
        folders.last().map(|f| f.id)
    );

    let act = panel.apply_shortcut(
        DialogsShortcut::SelectFolderSlot(2),
        &mut store,
        PEER_SAVED_MESSAGES,
    );
    assert_eq!(act, Some(DialogsPanelAction::SelectFolder(FOLDER_WORK)));
    assert_eq!(panel.active_filter, FOLDER_WORK);

    // Switch back to All Chats and test Up/Down + Enter + Ctrl+Tab + Ctrl+0.
    panel.apply_shortcut(
        DialogsShortcut::SelectFolderSlot(0),
        &mut store,
        PEER_SAVED_MESSAGES,
    );
    let top_two = store.dialog_slice(DialogListQuery::Filter(FilterId::ALL), 0, 3);
    let p0 = top_two.entries[0].peer_id;
    let p1 = top_two.entries[1].peer_id;

    // Ctrl+Tab advances from p0 -> p1.
    assert_eq!(
        panel.apply_shortcut(DialogsShortcut::NextChat, &mut store, p0),
        Some(DialogsPanelAction::SelectPeer(p1))
    );
    // Ctrl+Shift+Tab moves back from p1 -> p0.
    assert_eq!(
        panel.apply_shortcut(DialogsShortcut::PrevChat, &mut store, p1),
        Some(DialogsPanelAction::SelectPeer(p0))
    );

    // Down moves highlight, Enter activates.
    panel.inner.set_keyboard_selected_peer(Some(p0));
    panel.apply_shortcut(DialogsShortcut::MoveDown, &mut store, p0);
    assert_eq!(panel.inner.keyboard_selected_peer(), Some(p1));
    assert_eq!(
        panel.apply_shortcut(DialogsShortcut::ActivateSelected, &mut store, p0),
        Some(DialogsPanelAction::SelectPeer(p1))
    );

    // Ctrl+0 jumps to Saved Messages.
    assert_eq!(
        panel.apply_shortcut(DialogsShortcut::OpenSavedMessages, &mut store, p1),
        Some(DialogsPanelAction::SelectPeer(PEER_SAVED_MESSAGES))
    );

    // Ctrl+F activates search; Escape clears it.
    panel.apply_shortcut(DialogsShortcut::FocusSearch, &mut store, p1);
    assert!(panel.search_active);
    panel.search_query = "rust".to_owned();
    panel.apply_shortcut(DialogsShortcut::Escape, &mut store, p1);
    assert!(!panel.search_active);
    assert!(panel.search_query.is_empty());
}

#[test]
fn sidebar_badge_formatting_and_muted_rule() {
    assert_eq!(format_sidebar_badge(0), "");
    assert_eq!(format_sidebar_badge(7), "7");
    assert_eq!(format_sidebar_badge(999), "999");
    assert_eq!(format_sidebar_badge(1000), "99+");

    let state = FolderUnreadState {
        unread_chats: 5,
        muted_chats: 5,
    };
    assert_eq!(state.badge_display(true), (5, true));
    assert_eq!(state.badge_display(false), (0, false));

    let mixed = FolderUnreadState {
        unread_chats: 5,
        muted_chats: 2,
    };
    assert_eq!(mixed.badge_display(true), (5, false));
    assert_eq!(mixed.badge_display(false), (3, false));
}

#[test]
fn mock_data_contains_no_real_person_names() {
    let store = MockDialogsStore::new();
    let all = store.dialog_slice(DialogListQuery::Filter(FilterId::ALL), 0, 500);
    let banned = ["lovelace", "turing", "hopper", "durov", "torvalds", "linus"];
    for d in &all.entries {
        let lower = d.title.to_lowercase();
        for b in banned {
            assert!(
                !lower.contains(b),
                "Unexpected real name {b:?} in {:?}",
                d.title
            );
        }
    }
}

/// Generates `crates/tg-app/docs/dialogs.png` (Day) and `crates/tg-app/docs/dialogs_night.png`
/// (Night) showcasing the complete dialogs UI with real `tg-ui` ThemeManager palettes, Open Sans
/// fonts, `tg-lang` phrases, and `tg-style` icons.
///
/// Run with:
/// `cargo test -p tg-app dialogs::tests::capture_dialogs_screenshot -- --ignored --nocapture`
#[test]
#[ignore]
fn capture_dialogs_screenshot() {
    let docs_dir = tg_app_core::core_types::paths::workspace_app_docs_dir();
    capture_dialogs_for_mode(ThemeMode::Day, docs_dir.join("dialogs.png"));
    capture_dialogs_for_mode(ThemeMode::Night, docs_dir.join("dialogs_night.png"));
}

fn capture_dialogs_for_mode(mode: ThemeMode, out_path: PathBuf) {
    let done = Arc::new(AtomicBool::new(false));
    let done_check = Arc::clone(&done);

    struct ShowcaseApp {
        mode: ThemeMode,
        theme_manager: tg_ui::theme::ThemeManager,
        store_main: MockDialogsStore,
        panel_main: DialogsPanel,
        store_search: MockDialogsStore,
        panel_search: DialogsPanel,
        store_suggest: MockDialogsStore,
        panel_suggest: DialogsPanel,
        store_compact: MockDialogsStore,
        panel_compact: DialogsPanel,
        out_path: PathBuf,
        done: Arc<AtomicBool>,
        requested: bool,
        frames: u32,
    }

    impl ShowcaseApp {
        fn new(
            cc: &eframe::CreationContext<'_>,
            mode: ThemeMode,
            out_path: PathBuf,
            done: Arc<AtomicBool>,
        ) -> Self {
            let embedded = if mode.is_night() {
                tg_ui::tg_theme::EmbeddedType::Night
            } else {
                tg_ui::tg_theme::EmbeddedType::Default
            };
            let theme_manager = tg_ui::theme::ThemeManager::with_embedded(embedded);
            theme_manager.apply_to_egui(&cc.egui_ctx);

            let mut panel_main = DialogsPanel::new();
            panel_main.show_folder_tags = true;

            let mut panel_search = DialogsPanel::new();
            panel_search.search_active = true;
            panel_search.search_query = "rust".to_owned();

            let mut panel_suggest = DialogsPanel::new();
            panel_suggest.search_active = true;

            let panel_compact = DialogsPanel::new();

            Self {
                mode,
                theme_manager,
                store_main: MockDialogsStore::new(),
                panel_main,
                store_search: MockDialogsStore::new(),
                panel_search,
                store_suggest: MockDialogsStore::new(),
                panel_suggest,
                store_compact: MockDialogsStore::new(),
                panel_compact,
                out_path,
                done,
                requested: false,
                frames: 0,
            }
        }
    }

    impl eframe::App for ShowcaseApp {
        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            let ctx = ui.ctx().clone();
            let palette = Palette::for_mode(self.mode);
            let colors = crate::dialogs::layout::DialogColors::from_theme(&self.theme_manager);
            let strings = EnglishStrings;

            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(colors.window_bg()))
                .show(ui, |ui| {
                    let full = ui.max_rect();
                    let h = full.height();
                    let mut x = full.left();

                    // 1. Vertical Folders Sidebar (72px).
                    let sidebar_rect = Rect::from_min_size(
                        Pos2::new(x, full.top()),
                        Vec2::new(FILTERS_SIDEBAR_WIDTH, h),
                    );
                    ui.scope_builder(
                        UiBuilder::new()
                            .id_salt("showcase_sidebar")
                            .max_rect(sidebar_rect),
                        |ui| {
                            self.panel_main.show_vertical_sidebar(
                                ui,
                                &mut self.store_main,
                                &palette,
                            );
                        },
                    );
                    x += FILTERS_SIDEBAR_WIDTH;

                    // 2. Full Dialogs Column (332px) with Archive, Saved Messages, Typing, Draft, Tags.
                    let main_w = 332.0;
                    let main_rect =
                        Rect::from_min_size(Pos2::new(x, full.top()), Vec2::new(main_w, h));
                    ui.scope_builder(
                        UiBuilder::new()
                            .id_salt("showcase_main")
                            .max_rect(main_rect),
                        |ui| {
                            self.panel_main.show_column(
                                ui,
                                &mut self.store_main,
                                PeerId::from_chat(2001),
                                false,
                                false,
                                false,
                                false,
                                &palette,
                                &strings,
                            );
                        },
                    );
                    x += main_w;
                    ui.painter().line_segment(
                        [Pos2::new(x, full.top()), Pos2::new(x, full.bottom())],
                        Stroke::new(1.5, colors.divider()),
                    );

                    // 3. Search Results Column (336px) with query "rust" (Chats, Global, Highlighted Messages).
                    let search_w = 336.0;
                    let search_rect =
                        Rect::from_min_size(Pos2::new(x, full.top()), Vec2::new(search_w, h));
                    ui.scope_builder(
                        UiBuilder::new()
                            .id_salt("showcase_search")
                            .max_rect(search_rect),
                        |ui| {
                            self.panel_search.show_column(
                                ui,
                                &mut self.store_search,
                                PeerId::ZERO,
                                false,
                                false,
                                true,
                                false,
                                &palette,
                                &strings,
                            );
                        },
                    );
                    x += search_w;
                    ui.painter().line_segment(
                        [Pos2::new(x, full.top()), Pos2::new(x, full.bottom())],
                        Stroke::new(1.5, colors.divider()),
                    );

                    // 4. Horizontal Tabs + Search Suggestions Column (314px: Top Peers + Recent).
                    let suggest_w = 314.0;
                    let tabs_h = crate::dialogs::filters_menu::HORIZONTAL_TABS_HEIGHT;
                    let tabs_rect =
                        Rect::from_min_size(Pos2::new(x, full.top()), Vec2::new(suggest_w, tabs_h));
                    let suggest_rect = Rect::from_min_size(
                        Pos2::new(x, full.top() + tabs_h),
                        Vec2::new(suggest_w, (h - tabs_h).max(0.0)),
                    );
                    ui.scope_builder(
                        UiBuilder::new()
                            .id_salt("showcase_suggest_tabs")
                            .max_rect(tabs_rect),
                        |ui| {
                            let folders = self.store_suggest.chat_folders();
                            let _ = crate::dialogs::filters_menu::show_horizontal_folder_tabs(
                                ui,
                                &folders,
                                FilterId::ALL,
                                &palette,
                            );
                        },
                    );
                    ui.scope_builder(
                        UiBuilder::new()
                            .id_salt("showcase_suggest_body")
                            .max_rect(suggest_rect),
                        |ui| {
                            self.panel_suggest.show_column(
                                ui,
                                &mut self.store_suggest,
                                PeerId::ZERO,
                                false,
                                false,
                                true,
                                false,
                                &palette,
                                &strings,
                            );
                        },
                    );
                    x += suggest_w;
                    ui.painter().line_segment(
                        [Pos2::new(x, full.top()), Pos2::new(x, full.bottom())],
                        Stroke::new(1.5, colors.divider()),
                    );

                    // 5. Compact Narrow Avatar Rail (66px).
                    let compact_w = (full.right() - x).max(66.0);
                    let compact_rect =
                        Rect::from_min_size(Pos2::new(x, full.top()), Vec2::new(compact_w, h));
                    ui.scope_builder(
                        UiBuilder::new()
                            .id_salt("showcase_compact")
                            .max_rect(compact_rect),
                        |ui| {
                            self.panel_compact.show_column(
                                ui,
                                &mut self.store_compact,
                                PeerId::from_chat(2001),
                                false,
                                true,
                                true,
                                false,
                                &palette,
                                &strings,
                            );
                        },
                    );

                    // Subtle outer border for clean framing.
                    ui.painter().rect_stroke(
                        full.shrink(0.5),
                        CornerRadius::ZERO,
                        Stroke::new(1.0, colors.divider()),
                        egui::StrokeKind::Inside,
                    );
                });

            self.frames += 1;
            if self.done.load(Ordering::Acquire) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else if !self.requested && self.frames >= 8 {
                self.requested = true;
                let path = self.out_path.clone();
                let done = Arc::clone(&self.done);
                let ctx_cb = ctx.clone();
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                    egui::ScreenshotCallback::new(move |img| {
                        if let Some(parent) = path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        let [w, h] = img.size;
                        let mut raw = Vec::with_capacity(w * h * 4);
                        for px in &img.pixels {
                            let [r, g, b, a] = px.to_srgba_unmultiplied();
                            raw.extend_from_slice(&[r, g, b, a]);
                        }
                        if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, raw) {
                            let _ = buf.save(&path);
                        }
                        done.store(true, Ordering::Release);
                        ctx_cb.request_repaint();
                    }),
                ));
            } else {
                ctx.request_repaint();
            }
        }
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Rustgram — Dialogs Showcase")
            .with_inner_size([1120.0, 760.0])
            .with_decorations(false),
        #[cfg(target_os = "windows")]
        event_loop_builder: Some(Box::new(|builder| {
            use winit::platform::windows::EventLoopBuilderExtWindows;
            builder.with_any_thread(true);
        })),
        ..Default::default()
    };

    eframe::run_native(
        "Rustgram Dialogs Showcase",
        options,
        Box::new(move |cc| {
            Ok(Box::new(ShowcaseApp::new(
                cc,
                mode,
                out_path.clone(),
                Arc::clone(&done),
            )))
        }),
    )
    .expect("eframe screenshot run failed");

    assert!(
        done_check.load(Ordering::Acquire),
        "screenshot callback did not complete"
    );
}
