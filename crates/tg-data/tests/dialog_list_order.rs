//! `IndexedList` keeps the exact tdesktop order under random upserts and removals, and its
//! per-update cost is measured (TGB-3060).

use std::collections::{BTreeSet, HashMap};
use std::time::Instant;

use tg_data::{DialogKey, IndexedList, PeerId, SortMode, UserId};

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
}

fn key(id: u64) -> DialogKey {
    DialogKey::History(PeerId::from_user(UserId(id)))
}

fn upsert(list: &mut IndexedList, id: u64, sort_key: u64, name: &str) {
    let first: BTreeSet<char> = name.chars().next().into_iter().collect();
    let words: BTreeSet<String> = std::iter::once(name.to_owned()).collect();
    list.upsert(key(id), sort_key, name.to_owned(), words, first);
}

/// The order `IndexedList` promises: date mode by sort key descending, name mode by name
/// ascending, ties by key ascending.
fn expected(mode: SortMode, model: &HashMap<DialogKey, (u64, String)>) -> Vec<DialogKey> {
    let mut keys: Vec<DialogKey> = model.keys().copied().collect();
    match mode {
        SortMode::Date => keys.sort_by(|a, b| model[b].0.cmp(&model[a].0).then(a.cmp(b))),
        SortMode::Name => keys.sort_by(|a, b| model[a].1.cmp(&model[b].1).then(a.cmp(b))),
        SortMode::Add => unreachable!(),
    }
    keys
}

#[test]
fn order_matches_a_full_sort_after_random_updates() {
    for mode in [SortMode::Date, SortMode::Name] {
        let mut rng = Lcg(99);
        let mut list = IndexedList::new(mode, 0);
        let mut model = HashMap::new();
        for step in 0..4_000u32 {
            let id = rng.next() % 300;
            if rng.next().is_multiple_of(5) {
                list.remove(key(id));
                model.remove(&key(id));
            } else {
                // Few distinct values so equal sort keys and names (tie-breaks) are common.
                let sort_key = rng.next() % 40;
                let name = format!("n{}", rng.next() % 30);
                upsert(&mut list, id, sort_key, &name);
                model.insert(key(id), (sort_key, name));
            }
            if step.is_multiple_of(97) {
                assert_eq!(
                    list.keys(),
                    expected(mode, &model),
                    "{mode:?} at step {step}"
                );
            }
        }
        assert_eq!(list.keys(), expected(mode, &model), "{mode:?} final");
        assert_eq!(list.len(), model.len());
    }
}

#[test]
fn add_mode_keeps_insertion_order() {
    let mut list = IndexedList::new(SortMode::Add, 0);
    upsert(&mut list, 3, 1, "c");
    upsert(&mut list, 1, 9, "a");
    upsert(&mut list, 2, 5, "b");
    upsert(&mut list, 3, 100, "c");
    assert_eq!(list.keys(), [key(3), key(1), key(2)]);
    list.remove(key(1));
    assert_eq!(list.keys(), [key(3), key(2)]);
}

/// Prints the cost of filling a 5 000-dialog list and of 20 000 updates that move one dialog to
/// the top (a new message), the way every incoming message refreshes the chat list.
#[test]
fn upsert_timing() {
    const DIALOGS: u64 = 5_000;
    const UPDATES: u64 = 20_000;
    let mut list = IndexedList::new(SortMode::Date, 0);
    let mut rng = Lcg(5);

    let started = Instant::now();
    for id in 0..DIALOGS {
        upsert(&mut list, id, rng.next() << 16, "chat");
    }
    let fill = started.elapsed();

    let started = Instant::now();
    let mut top = u64::MAX >> 8;
    for _ in 0..UPDATES {
        top += 1;
        upsert(&mut list, rng.next() % DIALOGS, top, "chat");
    }
    let updates = started.elapsed();

    println!(
        "IndexedList: fill {DIALOGS} dialogs {:.1} ms, {UPDATES} move-to-top updates {:.1} ms \
         ({:.2} us/update)",
        fill.as_secs_f64() * 1e3,
        updates.as_secs_f64() * 1e3,
        updates.as_secs_f64() * 1e6 / UPDATES as f64
    );
    assert_eq!(list.len(), DIALOGS as usize);
}
