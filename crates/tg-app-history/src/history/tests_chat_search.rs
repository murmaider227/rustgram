//! In-chat search parity tests (`HistoryView::ComposeSearch`, Telegram Desktop 7.4.6).

use egui::Vec2;
use egui_kittest::kittest::Queryable;

use crate::history::compose_search::{ComposeSearch, ComposeSearchEvent};
use crate::history::tests_chat_search_snap::{
    MATCH_IDS, SEARCH_WORD, build_search_harness, open_and_type, seeded_store,
};
use tg_app_core::stand_in::theme::Palette;
use tg_app_core::stand_in::types::{MsgId, PeerId};

#[test]
fn typing_searches_the_chat_shows_the_count_and_jumps_to_the_newest_match() {
    let mut harness = build_search_harness(Palette::day(), Vec2::new(900.0, 640.0));
    open_and_type(&mut harness);
    let search = harness.state().widget.chat_search();
    assert_eq!(search.query, SEARCH_WORD);
    assert_eq!(search.total(), Some(3));
    assert_eq!(search.current(), 1);
    assert_eq!(search.counter_text(), "1 of 3");
    assert_eq!(search.highlight_query(), SEARCH_WORD);

    // The composer is replaced by the results bar; the top bar by the search field.
    assert_eq!(
        harness
            .query_all_by_role(egui::accesskit::Role::TextInput)
            .count(),
        1,
        "only the search field: the composer is hidden while searching"
    );
    harness.get_by_label("Close search");

    // The newest match is on screen.
    let newest = MsgId(MATCH_IDS[2]);
    let rect = harness
        .state()
        .widget
        .inner()
        .last_painted_bubble_rect(newest)
        .expect("newest match painted");
    assert!(rect.top() >= 0.0 && rect.bottom() <= 640.0, "{rect:?}");

    // Older arrow walks to the oldest match far above, which gets scrolled into view.
    harness.get_by_label("Older result").click();
    harness.step();
    harness.get_by_label("Older result").click();
    for _ in 0..4 {
        harness.step();
    }
    assert_eq!(
        harness.state().widget.chat_search().counter_text(),
        "3 of 3"
    );
    let oldest = harness
        .state()
        .widget
        .inner()
        .last_painted_bubble_rect(MsgId(MATCH_IDS[0]))
        .expect("oldest match painted");
    assert!(
        oldest.top() >= 0.0 && oldest.bottom() <= 640.0,
        "{oldest:?}"
    );

    // Back arrow restores the chat top bar and the composer.
    harness.get_by_label("Close search").click();
    for _ in 0..2 {
        harness.step();
    }
    assert!(!harness.state().widget.is_chat_search_open());
}

#[test]
fn search_debounces_typing_and_pages_older_results() {
    let peer = PeerId::from_chat(2001);
    let store = seeded_store(peer);
    let mut search = ComposeSearch::default();
    search.open("");
    search.query = SEARCH_WORD.to_owned();
    search.query_changed(10.0);
    assert_eq!(
        search.poll(10.5, &store, peer),
        None,
        "waits AutoSearchTimeout"
    );
    assert_eq!(
        search.poll(10.95, &store, peer),
        Some(ComposeSearchEvent::Jump(MsgId(MATCH_IDS[2])))
    );
    assert_eq!(search.step(-1, &store, peer), None, "no newer match");
    assert_eq!(
        search.step(1, &store, peer),
        Some(ComposeSearchEvent::Jump(MsgId(MATCH_IDS[1])))
    );
    // Re-polling the same request does not search again or move the position.
    search.submit();
    assert_eq!(search.poll(20.0, &store, peer), None);
    assert_eq!(search.current(), 2);

    // A query without matches reports "No results".
    search.query = "no-such-word".to_owned();
    search.submit();
    assert_eq!(search.poll(21.0, &store, peer), None);
    assert_eq!(search.total(), Some(0));
    assert_eq!(search.counter_text(), "No results");
}

#[test]
fn sender_filter_restricts_matches() {
    let peer = PeerId::from_chat(2001);
    let store = seeded_store(peer);
    let mut search = ComposeSearch::default();
    search.open(SEARCH_WORD);
    let _ = search.poll(0.0, &store, peer);
    assert_eq!(search.total(), Some(3));
    // Match 90_010 / 90_025 / 90_038 come from users 1002 / 1002 / 1003 (`1001 + i % 3`).
    search.set_from(Some((PeerId::from_user(1002), "Theo Park".to_owned())), 1.0);
    let ev = search.poll(2.0, &store, peer);
    assert_eq!(search.total(), Some(2));
    assert_eq!(ev, Some(ComposeSearchEvent::Jump(MsgId(MATCH_IDS[1]))));
}
