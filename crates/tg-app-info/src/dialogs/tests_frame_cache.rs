//! Chat-list frame cost: the counters proving that frames without changes rebuild nothing, the
//! checks that the kept rows never show something else than a fresh read, and a headless frame
//! timer over 500+ mock chats (`#[ignore]`, prints microseconds per frame).

use std::cell::Cell;
use std::time::Instant;

use egui::{Pos2, Rect, UiBuilder, Vec2};
use tg_history::LocalDateTime;

use crate::dialogs::backend::{
    ArchiveSummary, ChatFolder, DialogItem, DialogListQuery, DialogSlice, DialogsBackend,
    RecentPeerItem, SearchRequest, SearchResults, SearchTagChip, TopPeerItem,
};
use crate::dialogs::inner::{DialogsInnerWidget, variable_visible_row_range};
use crate::dialogs::layout::{elide_text, forget_elided_text, measure_text_i32};
use crate::dialogs::list_cache::{
    DialogsFrameStats, SNAPSHOT_MAX_AGE_SECS, TYPING_SNAPSHOT_MAX_AGE_SECS, running_row_tops,
    visible_rows_by_tops,
};
use crate::dialogs::mock::MockDialogsStore;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::{FilterId, PeerId};
use tg_ui::fonts::font_id_from_style;
use tg_ui::tg_style::st;

/// Chat-list column of a desktop window, 844 px tall.
const COLUMN: Vec2 = Vec2::new(420.0, 844.0);
const FRAME_DT: f64 = 1.0 / 60.0;
const ALL_CHATS: DialogListQuery = DialogListQuery::Filter(FilterId::ALL);

/// A backend that counts its changes like a live session: [`DialogsBackend::dialogs_version`]
/// moves on every change made through it. It also counts the list reads and the rows they
/// convert.
struct VersionedBackend {
    store: MockDialogsStore,
    version: u64,
    list_reads: Cell<u64>,
    rows_converted: Cell<u64>,
}

impl VersionedBackend {
    fn new(total: usize) -> Self {
        Self {
            store: MockDialogsStore::with_total_dialogs(total),
            version: 1,
            list_reads: Cell::new(0),
            rows_converted: Cell::new(0),
        }
    }

    fn change<R>(&mut self, change: impl FnOnce(&mut MockDialogsStore) -> R) -> R {
        self.version += 1;
        change(&mut self.store)
    }

    /// A change the backend fails to count.
    fn change_unreported<R>(&mut self, change: impl FnOnce(&mut MockDialogsStore) -> R) -> R {
        change(&mut self.store)
    }

    fn listed(&self) -> usize {
        self.store.dialog_slice(ALL_CHATS, 0, usize::MAX).total
    }
}

impl DialogsBackend for VersionedBackend {
    fn current_time(&self) -> LocalDateTime {
        self.store.current_time()
    }

    fn dialog_slice(&self, list: DialogListQuery, offset: usize, limit: usize) -> DialogSlice {
        let slice = self.store.dialog_slice(list, offset, limit);
        self.list_reads.set(self.list_reads.get() + 1);
        self.rows_converted
            .set(self.rows_converted.get() + slice.entries.len() as u64);
        slice
    }

    fn dialogs_version(&self) -> u64 {
        self.version
    }

    fn dialog_by_peer(&self, peer_id: PeerId) -> Option<DialogItem> {
        self.store.dialog_by_peer(peer_id)
    }

    fn chat_folders(&self) -> Vec<ChatFolder> {
        self.store.chat_folders()
    }

    fn reorder_folders(&mut self, order: &[FilterId]) {
        self.change(|s| s.reorder_folders(order));
    }

    fn add_peer_to_folder(&mut self, peer_id: PeerId, filter_id: FilterId) {
        self.change(|s| s.add_peer_to_folder(peer_id, filter_id));
    }

    fn remove_peer_from_folder(&mut self, peer_id: PeerId, filter_id: FilterId) {
        self.change(|s| s.remove_peer_from_folder(peer_id, filter_id));
    }

    fn mark_folder_read(&mut self, filter_id: FilterId) {
        self.change(|s| s.mark_folder_read(filter_id));
    }

    fn archive_summary(&self) -> Option<ArchiveSummary> {
        self.store.archive_summary()
    }

    fn archive_collapsed(&self) -> bool {
        self.store.archive_collapsed()
    }

    fn set_archive_collapsed(&mut self, collapsed: bool) {
        self.change(|s| s.set_archive_collapsed(collapsed));
    }

    fn toggle_archive(&mut self, peer_id: PeerId) -> bool {
        self.change(|s| s.toggle_archive(peer_id))
    }

    fn mark_archive_read(&mut self) {
        self.change(DialogsBackend::mark_archive_read);
    }

    fn toggle_pin(&mut self, peer_id: PeerId, list: DialogListQuery) -> bool {
        self.change(|s| s.toggle_pin(peer_id, list))
    }

    fn reorder_pinned(
        &mut self,
        list: DialogListQuery,
        from_index: usize,
        to_index: usize,
    ) -> bool {
        self.change(|s| s.reorder_pinned(list, from_index, to_index))
    }

    fn toggle_mute_dialog(&mut self, peer_id: PeerId) -> bool {
        self.change(|s| s.toggle_mute_dialog(peer_id))
    }

    fn mark_dialog_read(&mut self, peer_id: PeerId) {
        self.change(|s| s.mark_dialog_read(peer_id));
    }

    fn mark_dialog_unread(&mut self, peer_id: PeerId) {
        self.change(|s| s.mark_dialog_unread(peer_id));
    }

    fn clear_history(&mut self, peer_id: PeerId) {
        self.change(|s| s.clear_history(peer_id));
    }

    fn delete_dialog(&mut self, peer_id: PeerId) {
        self.change(|s| s.delete_dialog(peer_id));
    }

    fn search_dialogs(&self, request: &SearchRequest) -> SearchResults {
        self.store.search_dialogs(request)
    }

    fn top_peers(&self) -> Vec<TopPeerItem> {
        self.store.top_peers()
    }

    fn remove_top_peer(&mut self, peer_id: PeerId) {
        self.change(|s| s.remove_top_peer(peer_id));
    }

    fn recent_search_peers(&self) -> Vec<RecentPeerItem> {
        self.store.recent_search_peers()
    }

    fn add_recent_search_peer(&mut self, peer_id: PeerId) {
        self.change(|s| s.add_recent_search_peer(peer_id));
    }

    fn remove_recent_search_peer(&mut self, peer_id: PeerId) {
        self.change(|s| s.remove_recent_search_peer(peer_id));
    }

    fn clear_recent_search_peers(&mut self) {
        self.change(DialogsBackend::clear_recent_search_peers);
    }

    fn recent_hashtags(&self) -> Vec<String> {
        self.store.recent_hashtags()
    }

    fn remove_recent_hashtag(&mut self, tag: &str) {
        self.change(|s| s.remove_recent_hashtag(tag));
    }

    fn search_tags(&self) -> Vec<SearchTagChip> {
        self.store.search_tags()
    }

    fn saved_messages_peer_id(&self) -> PeerId {
        self.store.saved_messages_peer_id()
    }
}

/// Headless egui context that shows one [`DialogsInnerWidget`] per frame.
struct ListHarness {
    ctx: egui::Context,
    widget: DialogsInnerWidget,
    palette: Palette,
    time: f64,
}

impl ListHarness {
    fn new() -> Self {
        let ctx = egui::Context::default();
        tg_ui::fonts::register_tdesktop_fonts(&ctx);
        Self {
            ctx,
            widget: DialogsInnerWidget::new(),
            palette: Palette::for_mode(ThemeMode::Day),
            time: 0.0,
        }
    }

    fn input(&mut self, dt: f64, events: Vec<egui::Event>) -> egui::RawInput {
        self.time += dt;
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, COLUMN)),
            time: Some(self.time),
            events,
            ..Default::default()
        }
    }

    /// Runs one frame `dt` seconds after the previous one. Returns the UI pass time in seconds
    /// (layout and painting into shapes, without tessellation) and the frame's output.
    fn run(
        &mut self,
        backend: &mut dyn DialogsBackend,
        dt: f64,
        events: Vec<egui::Event>,
    ) -> (f64, egui::FullOutput) {
        let input = self.input(dt, events);
        let widget = &mut self.widget;
        let palette = &self.palette;
        let started = Instant::now();
        let output = self.ctx.run_ui(input, |ui| {
            ui.scope_builder(
                UiBuilder::new().max_rect(Rect::from_min_size(Pos2::ZERO, COLUMN)),
                |ui| {
                    widget.show(ui, backend, ALL_CHATS, PeerId::ZERO, false, false, palette);
                },
            );
        });
        (started.elapsed().as_secs_f64(), output)
    }

    fn frame_after(
        &mut self,
        backend: &mut dyn DialogsBackend,
        dt: f64,
        events: Vec<egui::Event>,
    ) -> f64 {
        let (elapsed, output) = self.run(backend, dt, events);
        output.drop_without_applying_deltas();
        elapsed
    }

    fn frame(&mut self, backend: &mut dyn DialogsBackend, events: Vec<egui::Event>) -> f64 {
        self.frame_after(backend, FRAME_DT, events)
    }

    /// Runs a frame at the time of the previous one and returns what it painted, as meshes.
    fn painted(&mut self, backend: &mut dyn DialogsBackend) -> Vec<(Rect, egui::Mesh)> {
        let (_, mut output) = self.run(backend, 0.0, Vec::new());
        let shapes = std::mem::take(&mut output.shapes);
        let pixels_per_point = output.pixels_per_point;
        output.drop_without_applying_deltas();
        self.ctx
            .tessellate(shapes, pixels_per_point)
            .into_iter()
            .filter_map(|clipped| match clipped.primitive {
                egui::epaint::Primitive::Mesh(mesh) => Some((clipped.clip_rect, mesh)),
                egui::epaint::Primitive::Callback(_) => None,
            })
            .collect()
    }

    /// Scrolls the list by `delta_y` points (negative moves the content up) with the pointer
    /// over the column.
    fn wheel(delta_y: f32) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(Pos2::new(COLUMN.x * 0.5, COLUMN.y * 0.5)),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: Vec2::new(0.0, delta_y),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Unknown,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    /// Scrolls down until every chat of the list is loaded into the widget's window, then
    /// lets the scroll animation end.
    fn scroll_to_end(&mut self, backend: &mut dyn DialogsBackend, total: usize) {
        for _ in 0..4000 {
            if self.widget.loaded_limit() >= total {
                break;
            }
            self.frame(backend, Self::wheel(-2400.0));
        }
        for _ in 0..30 {
            self.frame(backend, Self::wheel(-2400.0));
        }
        for _ in 0..60 {
            self.frame(backend, Vec::new());
        }
    }

    fn stats(&self) -> DialogsFrameStats {
        self.widget.frame_stats()
    }
}

/// The chat the fifth visible row shows: far enough down to be an ordinary, unpinned chat.
fn fifth_row_peer(backend: &dyn DialogsBackend) -> PeerId {
    backend.dialog_slice(ALL_CHATS, 0, 16).entries[5].peer_id
}

#[test]
fn idle_frames_read_no_list_and_prepare_no_rows() {
    let mut backend = VersionedBackend::new(1000);
    let listed = backend.listed();
    assert!(listed >= 900, "{listed} chats in the main list");
    let mut harness = ListHarness::new();
    harness.scroll_to_end(&mut backend, listed);
    assert!(harness.widget.loaded_limit() >= listed);

    // 60 frames while nothing changes: no chat is converted and no row text is prepared.
    let reads = backend.list_reads.get();
    let converted = backend.rows_converted.get();
    let stats = harness.stats();
    for _ in 0..60 {
        harness.frame_after(&mut backend, 0.001, Vec::new());
    }
    assert_eq!(backend.list_reads.get(), reads, "list reads on idle frames");
    assert_eq!(
        backend.rows_converted.get(),
        converted,
        "rows converted on idle frames"
    );
    assert_eq!(harness.stats(), stats, "widget work on idle frames");

    // Two seconds at 60 fps: the list is read once per snapshot age (the clock alone can change
    // a row), and since the rows come back unchanged still nothing is prepared.
    let typing_rows = backend
        .store
        .dialog_slice(ALL_CHATS, 0, usize::MAX)
        .entries
        .iter()
        .any(|d| d.typing.is_some());
    let max_age = if typing_rows {
        TYPING_SNAPSHOT_MAX_AGE_SECS
    } else {
        SNAPSHOT_MAX_AGE_SECS
    };
    for _ in 0..120 {
        harness.frame(&mut backend, Vec::new());
    }
    let reads_in_two_seconds = backend.list_reads.get() - reads;
    let allowed = (2.0 / max_age).ceil() as u64 + 1;
    assert!(
        (1..=allowed).contains(&reads_in_two_seconds),
        "{reads_in_two_seconds} list reads in 2 s, at most {allowed} expected"
    );
    assert_eq!(
        harness.stats().rows_prepared,
        stats.rows_prepared,
        "rows prepared on idle frames"
    );
}

#[test]
fn a_change_is_painted_on_the_next_frame_and_prepares_only_its_row() {
    let mut backend = VersionedBackend::new(1000);
    let mut harness = ListHarness::new();
    for _ in 0..10 {
        harness.frame_after(&mut backend, 0.001, Vec::new());
    }
    let peer = fifth_row_peer(&backend);
    let reads = backend.list_reads.get();
    let stats = harness.stats();

    backend.toggle_mute_dialog(peer);
    harness.frame_after(&mut backend, 0.001, Vec::new());

    assert_eq!(
        backend.list_reads.get(),
        reads + 1,
        "the list is read again"
    );
    assert_eq!(
        harness.stats().rows_prepared,
        stats.rows_prepared + 1,
        "only the changed row is prepared again"
    );

    // And it stays quiet afterwards.
    let stats = harness.stats();
    for _ in 0..10 {
        harness.frame_after(&mut backend, 0.001, Vec::new());
    }
    assert_eq!(harness.stats(), stats);
}

#[test]
fn an_uncounted_change_is_shown_once_the_snapshot_is_old() {
    let mut backend = VersionedBackend::new(600);
    let mut harness = ListHarness::new();
    for _ in 0..10 {
        harness.frame_after(&mut backend, 0.001, Vec::new());
    }
    let peer = fifth_row_peer(&backend);
    let stats = harness.stats();

    backend.change_unreported(|store| store.toggle_mute_dialog(peer));
    // Never shown stale for longer than the snapshot age, whatever the backend counts.
    harness.frame_after(&mut backend, SNAPSHOT_MAX_AGE_SECS, Vec::new());

    assert_eq!(harness.stats().list_reads, stats.list_reads + 1);
    assert_eq!(harness.stats().rows_prepared, stats.rows_prepared + 1);
}

#[test]
fn a_backend_without_versions_is_read_every_frame_but_keeps_its_row_text() {
    let mut store = MockDialogsStore::with_total_dialogs(600);
    let mut harness = ListHarness::new();
    for _ in 0..10 {
        harness.frame_after(&mut store, 0.001, Vec::new());
    }
    let stats = harness.stats();
    for _ in 0..10 {
        harness.frame_after(&mut store, 0.001, Vec::new());
    }
    assert_eq!(harness.stats().list_reads, stats.list_reads + 10);
    assert_eq!(harness.stats().rows_prepared, stats.rows_prepared);
}

#[test]
fn growing_the_loaded_window_reads_the_list_again() {
    let mut backend = VersionedBackend::new(1000);
    let listed = backend.listed();
    let mut harness = ListHarness::new();
    harness.frame(&mut backend, Vec::new());
    let first_page = harness.widget.loaded_limit();
    harness.scroll_to_end(&mut backend, listed);
    assert!(harness.widget.loaded_limit() > first_page);
    // The last row of the list is reachable: every page was read as the window grew.
    assert!(harness.widget.loaded_limit() >= listed);
    let bottom = harness.widget.scroll_offset_y();
    assert!(
        bottom > 60.0 * (listed as f32 - 20.0),
        "scrolled to {bottom}"
    );
}

/// A frame painted from the kept rows, prepared text and remembered elisions is the frame a
/// new widget with nothing kept paints.
#[test]
fn kept_rows_paint_exactly_what_a_fresh_read_paints() {
    let mut backend = VersionedBackend::new(600);
    let mut harness = ListHarness::new();
    for _ in 0..10 {
        harness.frame(&mut backend, Vec::new());
    }
    let stats = harness.stats();
    let warm = harness.painted(&mut backend);
    assert_eq!(harness.stats(), stats, "the warm frame used only kept data");
    assert!(warm.len() > 1, "the list painted something");

    harness.widget = DialogsInnerWidget::new();
    forget_elided_text(&harness.ctx);
    let cold = harness.painted(&mut backend);
    assert!(
        harness.stats().rows_prepared >= 10,
        "the cold frame prepared its rows"
    );

    assert_eq!(warm.len(), cold.len(), "number of painted meshes");
    for (idx, (w, c)) in warm.iter().zip(&cold).enumerate() {
        assert_eq!(w.0, c.0, "clip rect of mesh {idx}");
        assert_eq!(w.1.texture_id, c.1.texture_id, "texture of mesh {idx}");
        assert_eq!(w.1.indices, c.1.indices, "indices of mesh {idx}");
        assert_eq!(w.1.vertices, c.1.vertices, "vertices of mesh {idx}");
    }
}

/// The whole column (folder tabs above the list) on a backend that counts its changes: the
/// folder list, whose unread counters walk every chat, is read when something changed.
#[test]
fn folder_tabs_read_the_folder_list_only_when_the_backend_changed() {
    use crate::dialogs::widget::DialogsPanel;
    use tg_app_core::stand_in::strings::EnglishStrings;

    let mut backend = VersionedBackend::new(600);
    let harness = ListHarness::new();
    let mut panel = DialogsPanel::new();
    let palette = Palette::for_mode(ThemeMode::Day);
    let mut time = 0.0_f64;
    let mut frame = |panel: &mut DialogsPanel, backend: &mut VersionedBackend, dt: f64| {
        time += dt;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, COLUMN)),
            time: Some(time),
            ..Default::default()
        };
        harness
            .ctx
            .run_ui(input, |ui| {
                ui.scope_builder(
                    UiBuilder::new().max_rect(Rect::from_min_size(Pos2::ZERO, COLUMN)),
                    |ui| {
                        panel.show_column(
                            ui,
                            backend,
                            PeerId::ZERO,
                            true,
                            false,
                            true,
                            false,
                            &palette,
                            &EnglishStrings,
                        );
                    },
                );
            })
            .drop_without_applying_deltas();
    };

    for _ in 0..5 {
        frame(&mut panel, &mut backend, 0.001);
    }
    let reads = panel.folder_list_reads();
    assert!(reads >= 1);
    for _ in 0..60 {
        frame(&mut panel, &mut backend, 0.001);
    }
    assert_eq!(
        panel.folder_list_reads(),
        reads,
        "folder reads on idle frames"
    );

    let peer = fifth_row_peer(&backend);
    backend.mark_dialog_unread(peer);
    frame(&mut panel, &mut backend, 0.001);
    assert_eq!(
        panel.folder_list_reads(),
        reads + 1,
        "read again after a change"
    );

    // And never kept longer than the snapshot age.
    frame(&mut panel, &mut backend, SNAPSHOT_MAX_AGE_SECS);
    assert_eq!(panel.folder_list_reads(), reads + 2);
}

#[test]
fn binary_searched_visible_rows_match_the_walk_from_the_first_row() {
    // 62 px rows with a 72 px tagged row here and there, like a list with folder tags.
    let mut seed = 0x2545_F491_4F6C_DD1D_u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for rows in [0_usize, 1, 2, 17, 400, 2000] {
        let heights: Vec<f32> = (0..rows)
            .map(|_| if next() % 5 == 0 { 72.0 } else { 62.0 })
            .collect();
        let tops = running_row_tops(&heights);
        let total: f32 = heights.iter().sum();
        assert_eq!(tops.last().copied().unwrap_or(0.0), total);
        for _ in 0..400 {
            let top = (next() % 200_000) as f32 / 1.5 - 300.0;
            let height = (next() % 3000) as f32 / 2.0 - 10.0;
            let viewport = (top.max(0.0), (top + height).max(0.0));
            assert_eq!(
                visible_rows_by_tops(viewport.0, viewport.1, &tops),
                variable_visible_row_range(viewport.0, viewport.1, &heights),
                "{rows} rows, viewport {viewport:?}"
            );
        }
        // Viewport edges exactly on row edges.
        for edge in tops.iter().copied().take(40) {
            for bottom in [edge, edge + 62.0, edge + 844.0] {
                assert_eq!(
                    visible_rows_by_tops(edge, bottom, &tops),
                    variable_visible_row_range(edge, bottom, &heights),
                    "{rows} rows, viewport {edge}..{bottom}"
                );
            }
        }
    }
}

/// The previous elision: one measurement per character from the start.
fn elide_every_prefix(text: &str, max_width: i32, mut measure: impl FnMut(&str) -> i32) -> String {
    if max_width <= 0 || text.is_empty() {
        return String::new();
    }
    if measure(text) <= max_width {
        return text.to_owned();
    }
    let ellipsis_w = measure("\u{2026}");
    if ellipsis_w > max_width {
        return String::new();
    }
    let mut best = 0usize;
    for (idx, ch) in text.char_indices() {
        let end = idx + ch.len_utf8();
        if measure(&text[..end]) + ellipsis_w <= max_width {
            best = end;
        } else {
            break;
        }
    }
    format!("{}\u{2026}", &text[..best])
}

/// Titles and previews are cut at the same character as before, in the real chat-list fonts.
#[test]
fn single_pass_elision_cuts_where_measuring_every_prefix_did() {
    const CORPUS: &[&str] = &[
        "Planning sync",
        "Weekend hiking club: gear list, routes and car sharing for Saturday",
        "illlliiii.....,,,,'''' narrow letters first, then MMMMWWWW wide ones",
        "MMMMWWWWmmmmwwww wide letters first, then illlliiii narrow ones",
        "Привет! Завтра созвон в десять утра по поводу релиза, не опаздывай",
        "日本語のタイトルはとても長い場合があります、省略されるはずです",
        "mixed ascii и кириллица と 日本語 together in one preview line",
        "🙂🙃 emoji 🚀 in the middle 🎉 and at the end 🎈🎈🎈 of a long preview",
        "a\u{0301}e\u{0301}i\u{0301} combining marks o\u{0301}u\u{0301} all over the string",
        "two lines\nin one message preview that is long enough to be cut somewhere",
        "tabs\tand  double  spaces   and trailing spaces      in a row of text   ",
        "x",
    ];
    let mut harness = ListHarness::new();
    let mut store = MockDialogsStore::with_total_dialogs(40);
    // Fonts registered on a context apply from the next frame.
    harness.frame(&mut store, Vec::new());
    harness.frame(&mut store, Vec::new());

    let mut compared = 0usize;
    let mut cut = 0usize;
    let input = harness.input(FRAME_DT, Vec::new());
    harness
        .ctx
        .clone()
        .run_ui(input, |ui| {
            let painter = ui.painter();
            for font in [
                font_id_from_style(st::semibold_font),
                font_id_from_style(st::dialogs_text_font),
            ] {
                for text in CORPUS {
                    let full = measure_text_i32(painter, text, font.clone());
                    for max_width in (-2..=full + 3).step_by(3) {
                        let expected = elide_every_prefix(text, max_width, |s| {
                            measure_text_i32(painter, s, font.clone())
                        });
                        let got = elide_text(painter, text, max_width, &font);
                        assert_eq!(got, expected, "{text:?} at {max_width}px in {font:?}");
                        // Remembered result of the same call.
                        assert_eq!(elide_text(painter, text, max_width, &font), expected);
                        compared += 1;
                        cut += usize::from(expected.ends_with('\u{2026}'));
                    }
                }
            }
        })
        .drop_without_applying_deltas();
    assert!(compared > 2000, "{compared} widths compared");
    assert!(cut > 1500, "{cut} of them elided");
}

fn median_micros(samples: &mut [f64]) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2] * 1e6
}

fn mean_micros(samples: &[f64]) -> f64 {
    samples.iter().sum::<f64>() / samples.len() as f64 * 1e6
}

/// Times `FRAMES` frames in three states: at the top of the list (first page loaded), scrolled
/// to the end (every chat loaded) while idle, and at the end while scrolling.
fn time_list_frames(label: &str, backend: &mut dyn DialogsBackend, listed: usize) {
    const FRAMES: usize = 240;
    let mut harness = ListHarness::new();
    for _ in 0..30 {
        harness.frame(backend, Vec::new());
    }
    let mut top_idle: Vec<f64> = (0..FRAMES)
        .map(|_| harness.frame(backend, Vec::new()))
        .collect();

    harness.scroll_to_end(backend, listed);
    let loaded = harness.widget.loaded_limit();
    let mut end_idle: Vec<f64> = (0..FRAMES)
        .map(|_| harness.frame(backend, Vec::new()))
        .collect();
    let mut end_scroll: Vec<f64> = (0..FRAMES)
        .map(|i| {
            let delta = if (i / 60) % 2 == 0 { 40.0 } else { -40.0 };
            harness.frame(backend, ListHarness::wheel(delta))
        })
        .collect();

    println!(
        "{label:<22} chats={listed:>4} loaded_limit={loaded:>4} | top idle: mean {:>8.1} us, median {:>8.1} us \
         | end idle: mean {:>8.1} us, median {:>8.1} us | end scrolling: mean {:>8.1} us, median {:>8.1} us",
        mean_micros(&top_idle),
        median_micros(&mut top_idle),
        mean_micros(&end_idle),
        median_micros(&mut end_idle),
        mean_micros(&end_scroll),
        median_micros(&mut end_scroll),
    );
}

/// Prints the chat-list frame time for 500, 1000 and 2000 mock chats, once on a backend that
/// counts its changes (as the live session does) and once on the plain mock store, which is
/// read every frame.
///
/// `cargo test -p tg-app-info --lib dialogs::tests_frame_cache::chat_list_frame_timings -- --ignored --nocapture`
#[test]
#[ignore]
fn chat_list_frame_timings() {
    for total in [500_usize, 1000, 2000] {
        let mut versioned = VersionedBackend::new(total);
        let listed = versioned.listed();
        time_list_frames("versioned backend", &mut versioned, listed);
        let mut store = MockDialogsStore::with_total_dialogs(total);
        time_list_frames("unversioned mock store", &mut store, listed);
    }
}
