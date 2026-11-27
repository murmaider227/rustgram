//! Port of `dialogs/dialogs_common.h`, `dialogs/dialogs_key.h`,
//! `dialogs/dialogs_entry.h`, `dialogs/dialogs_entry.cpp`,
//! `dialogs/dialogs_pinned_list.cpp`, `dialogs/dialogs_indexed_list.h`,
//! `dialogs/dialogs_indexed_list.cpp`, `dialogs/dialogs_main_list.h`,
//! `dialogs/dialogs_main_list.cpp`, `data/data_folder.h`, `data/data_folder.cpp`,
//! `data/data_chat_filters.h`, and `data/data_chat_filters.cpp`.
//!
//! Implements dialog keys, exact tdesktop 64-bit sort keys (`DialogPosFromDate`,
//! `FixedOnTopDialogPos`, `PinnedDialogPos`, `UnreadOnTopDialogPos`), pinned
//! lists, letter-indexed dialog lists, main/archive lists, unread state & badge
//! calculations, and chat filters (custom folders) matching logic.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::{Add, AddAssign, Sub, SubAssign};

use bitflags::bitflags;

use crate::history::History;
use crate::id::{ARCHIVE_FOLDER_ID, FilterId, FolderId, MsgId, PeerId, TimeId};
use crate::peer::{Peer, split_search_words};
use crate::text::TextWithEntities;

/// Fixed-on-top index for the Archive folder (`Entry::kArchiveFixOnTopIndex`).
pub const ARCHIVE_FIX_ON_TOP_INDEX: i32 = 1;

/// Fixed-on-top index for top-promoted dialogs (`Entry::kTopPromotionFixOnTopIndex`).
pub const TOP_PROMOTION_FIX_ON_TOP_INDEX: i32 = 2;

/// Sort mode for `IndexedList` (`Dialogs::SortMode`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum SortMode {
    /// Sort descending by 64-bit dialog sort key (fixed > pinned > unread > date).
    #[default]
    Date,
    /// Sort ascending alphabetically by display name.
    Name,
    /// Preserve insertion order.
    Add,
}

/// Identifies an entry in a dialogs list (`Dialogs::Key`).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum DialogKey {
    /// A peer conversation (`History`).
    History(PeerId),
    /// A dialog folder (e.g. Archive `FolderId = 1`).
    Folder(FolderId),
    /// A forum topic inside a peer's forum.
    ForumTopic {
        /// Forum peer ID.
        peer: PeerId,
        /// Topic root message ID.
        root_id: MsgId,
    },
    /// A Saved Messages / monoforum sublist.
    SavedSublist {
        /// Owning monoforum or self peer ID.
        parent: PeerId,
        /// Sublist peer ID.
        peer: PeerId,
    },
}

impl DialogKey {
    /// Returns the `PeerId` if this key is `DialogKey::History`.
    #[inline]
    pub const fn as_history_peer(self) -> Option<PeerId> {
        match self {
            Self::History(peer) => Some(peer),
            _ => None,
        }
    }

    /// Returns the `FolderId` if this key is `DialogKey::Folder`.
    #[inline]
    pub const fn as_folder_id(self) -> Option<FolderId> {
        match self {
            Self::Folder(id) => Some(id),
            _ => None,
        }
    }
}

/// Computes the date-based 64-bit sort key (`DialogPosFromDate` in `dialogs_entry.cpp`).
///
/// `shift` is a monotonically increasing tie-breaker (`++DialogsPosToTopShift`).
/// A `date <= 0` yields `0`.
#[inline]
pub const fn dialog_pos_from_date(date: TimeId, shift: u32) -> u64 {
    if date <= 0 {
        0
    } else {
        ((date as u32 as u64) << 32) | (shift as u64)
    }
}

/// Computes the fixed-on-top sort key (`FixedOnTopDialogPos` in `dialogs_entry.cpp`).
///
/// Exact C++ formula: `0xFFFFFFFFFFFF000FULL - index`.
/// Higher sort keys appear earlier (closer to the top of the list), so
/// `ARCHIVE_FIX_ON_TOP_INDEX (1)` sits above `TOP_PROMOTION_FIX_ON_TOP_INDEX (2)`.
#[inline]
pub const fn fixed_on_top_dialog_pos(index: i32) -> u64 {
    0xFFFF_FFFF_FFFF_000F_u64.wrapping_sub(index as i64 as u64)
}

/// Computes the pinned dialog sort key (`PinnedDialogPos` in `dialogs_entry.cpp`).
///
/// Exact C++ formula: `0xFFFFFFFF000000FFULL - pinnedIndex` (1-based `pinned_index`,
/// so `pinned_index = 1` sorts above `pinned_index = 2`, and all pinned dialogs sort
/// below fixed-on-top entries and above regular date-sorted entries).
#[inline]
pub const fn pinned_dialog_pos(pinned_index: i32) -> u64 {
    0xFFFF_FFFF_0000_00FF_u64.wrapping_sub(pinned_index as i64 as u64)
}

/// Promotes a date-based sort key to the "unread on top" band (`UnreadOnTopDialogPos`).
///
/// Exact C++ formula: `sortKeyByDate | 0x8000000000000000ULL`.
#[inline]
pub const fn unread_on_top_dialog_pos(sort_key_by_date: u64) -> u64 {
    sort_key_by_date | 0x8000_0000_0000_0000_u64
}

/// Computes the full dialog sort key combining fixed-on-top, pinned, unread-on-top,
/// and date ordering (`Entry::updateChatListSortPosition` + `Entry::computeSortPosition`).
#[inline]
pub const fn compute_dialog_sort_key(
    date: TimeId,
    shift: u32,
    fixed_on_top_index: i32,
    pinned_index: i32,
    unread_on_top: bool,
) -> u64 {
    if fixed_on_top_index > 0 {
        return fixed_on_top_dialog_pos(fixed_on_top_index);
    }
    if pinned_index > 0 {
        return pinned_dialog_pos(pinned_index);
    }
    let by_date = dialog_pos_from_date(date, shift);
    if unread_on_top && by_date != 0 {
        unread_on_top_dialog_pos(by_date)
    } else {
        by_date
    }
}

/// Aggregated unread counters for a dialog, folder, filter, or main list
/// (`Dialogs::UnreadState` in `dialogs_common.h`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct UnreadState {
    /// Total unread messages across muted + unmuted chats.
    pub messages: i32,
    /// Unread messages in muted chats.
    pub messages_muted: i32,
    /// Total chats with unread messages (`unread_count > 0`).
    pub chats: i32,
    /// Muted chats with unread messages.
    pub chats_muted: i32,
    /// Total chats explicitly marked unread (with 0 unread messages).
    pub marks: i32,
    /// Muted chats explicitly marked unread.
    pub marks_muted: i32,
    /// Total chats with unread reactions.
    pub reactions: i32,
    /// Muted chats with unread reactions.
    pub reactions_muted: i32,
    /// Total chats with unread mentions.
    pub mentions: i32,
    /// Total chats with unread poll votes.
    pub polls: i32,
    /// Muted chats with unread poll votes.
    pub polls_muted: i32,
    /// Whether this unread state is known / initialized.
    pub known: bool,
}

impl UnreadState {
    /// Returns `true` if all numeric counters are zero.
    pub const fn is_empty(&self) -> bool {
        self.messages == 0
            && self.chats == 0
            && self.marks == 0
            && self.reactions == 0
            && self.mentions == 0
            && self.polls == 0
    }

    /// Returns `true` if there is any unmuted unread activity used by `unread-on-top`
    /// sorting (`Entry::hasUnreadUnmutedForSort`).
    pub const fn has_unread_unmuted_for_sort(&self) -> bool {
        (self.messages > self.messages_muted)
            || (self.marks > self.marks_muted)
            || (self.reactions > self.reactions_muted)
            || (self.mentions > 0)
    }
}

/// Saturating: the counters come from the server (`dialog.unread_count`), and a hostile or buggy
/// total must not overflow (tdesktop's `int` math is undefined behaviour there).
impl AddAssign for UnreadState {
    fn add_assign(&mut self, rhs: Self) {
        self.messages = self.messages.saturating_add(rhs.messages);
        self.messages_muted = self.messages_muted.saturating_add(rhs.messages_muted);
        self.chats = self.chats.saturating_add(rhs.chats);
        self.chats_muted = self.chats_muted.saturating_add(rhs.chats_muted);
        self.marks = self.marks.saturating_add(rhs.marks);
        self.marks_muted = self.marks_muted.saturating_add(rhs.marks_muted);
        self.reactions = self.reactions.saturating_add(rhs.reactions);
        self.reactions_muted = self.reactions_muted.saturating_add(rhs.reactions_muted);
        self.mentions = self.mentions.saturating_add(rhs.mentions);
        self.polls = self.polls.saturating_add(rhs.polls);
        self.polls_muted = self.polls_muted.saturating_add(rhs.polls_muted);
    }
}

/// Saturating, like [`AddAssign`].
impl SubAssign for UnreadState {
    fn sub_assign(&mut self, rhs: Self) {
        self.messages = self.messages.saturating_sub(rhs.messages);
        self.messages_muted = self.messages_muted.saturating_sub(rhs.messages_muted);
        self.chats = self.chats.saturating_sub(rhs.chats);
        self.chats_muted = self.chats_muted.saturating_sub(rhs.chats_muted);
        self.marks = self.marks.saturating_sub(rhs.marks);
        self.marks_muted = self.marks_muted.saturating_sub(rhs.marks_muted);
        self.reactions = self.reactions.saturating_sub(rhs.reactions);
        self.reactions_muted = self.reactions_muted.saturating_sub(rhs.reactions_muted);
        self.mentions = self.mentions.saturating_sub(rhs.mentions);
        self.polls = self.polls.saturating_sub(rhs.polls);
        self.polls_muted = self.polls_muted.saturating_sub(rhs.polls_muted);
    }
}

impl Add for UnreadState {
    type Output = Self;

    fn add(mut self, rhs: Self) -> Self::Output {
        self += rhs;
        self
    }
}

impl Sub for UnreadState {
    type Output = Self;

    fn sub(mut self, rhs: Self) -> Self::Output {
        self -= rhs;
        self
    }
}

/// Visual badge indicators computed from an `UnreadState` (`Dialogs::BadgesState`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct BadgesState {
    /// Numeric badge count (`0` when displaying a dot for a single unread mark).
    pub unread_counter: i32,
    /// Whether an unread badge (number or dot) should be shown.
    pub unread: bool,
    /// Whether the unread badge should be rendered in muted/grey style.
    pub unread_muted: bool,
    /// Whether the `@` mention badge should be shown.
    pub mention: bool,
    /// Whether the `@` mention badge should be muted.
    pub mention_muted: bool,
    /// Whether the heart reaction badge should be shown.
    pub reaction: bool,
    /// Whether the reaction badge should be muted.
    pub reaction_muted: bool,
    /// Whether the poll vote badge should be shown.
    pub poll: bool,
    /// Whether the poll vote badge should be muted.
    pub poll_muted: bool,
}

impl BadgesState {
    /// Returns `true` if no badge indicator is active.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        !self.unread && !self.mention && !self.reaction && !self.poll
    }
}

/// Whether badges count unread chats or unread messages (`Dialogs::CountInBadge`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum CountInBadge {
    /// Use `BadgeSettings::count_unread_messages`.
    #[default]
    Default,
    /// Always count unread chats.
    Chats,
    /// Always count unread messages.
    Messages,
}

/// Whether badges include muted chats (`Dialogs::IncludeInBadge`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum IncludeInBadge {
    /// Use `BadgeSettings::include_muted_counter`.
    #[default]
    Default,
    /// Only unmuted unread items.
    Unmuted,
    /// Both unmuted and muted unread items.
    All,
    /// Unmuted if any exist, otherwise fall back to all (muted) items.
    UnmutedOrAll,
}

/// Global user settings controlling app/filter badge computation
/// (mirrors `Core::Settings::countUnreadMessages` and `includeMutedCounter`).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct BadgeSettings {
    /// `true` = count unread messages; `false` = count unread chats.
    pub count_unread_messages: bool,
    /// `true` = include muted chats in the badge counter.
    pub include_muted_counter: bool,
}

impl Default for BadgeSettings {
    fn default() -> Self {
        Self {
            count_unread_messages: true,
            include_muted_counter: true,
        }
    }
}

/// Computes `BadgesState` from `UnreadState` using the exact rules of
/// `Dialogs::BadgesForUnread` (`dialogs_entry.cpp`).
pub const fn badges_for_unread(
    state: &UnreadState,
    count: CountInBadge,
    include: IncludeInBadge,
    settings: BadgeSettings,
) -> BadgesState {
    let count_messages = matches!(count, CountInBadge::Messages)
        || (matches!(count, CountInBadge::Default) && settings.count_unread_messages);

    let counter_full = state.marks
        + if count_messages {
            state.messages
        } else {
            state.chats
        };
    let counter_muted = state.marks_muted
        + if count_messages {
            state.messages_muted
        } else {
            state.chats_muted
        };
    let unread_muted_all = counter_full <= counter_muted;

    let include_muted = matches!(include, IncludeInBadge::All)
        || (matches!(include, IncludeInBadge::UnmutedOrAll) && unread_muted_all)
        || (matches!(include, IncludeInBadge::Default) && settings.include_muted_counter);

    let marks = state.marks - if include_muted { 0 } else { state.marks_muted };
    let counter = counter_full - if include_muted { 0 } else { counter_muted };
    let is_single_mark = counter == 1 && marks == 1;

    BadgesState {
        unread_counter: if is_single_mark { 0 } else { counter },
        unread: counter > 0,
        unread_muted: include_muted && counter <= counter_muted,
        mention: state.mentions > 0,
        mention_muted: false,
        reaction: state.reactions > 0,
        reaction_muted: state.reactions <= state.reactions_muted,
        poll: state.polls > 0,
        poll_muted: state.polls <= state.polls_muted,
    }
}

/// Ordered list of pinned dialogs with a configurable cap (`Dialogs::PinnedList`).
///
/// Indices exposed to entries are 1-based (`1` = top-most pin, `0` = unpinned).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PinnedList {
    filter_id: FilterId,
    limit: usize,
    order: Vec<DialogKey>,
}

impl PinnedList {
    /// Creates a new `PinnedList` with at least `limit.max(1)` capacity.
    pub fn new(filter_id: FilterId, limit: usize) -> Self {
        Self {
            filter_id,
            limit: limit.max(1),
            order: Vec::new(),
        }
    }

    /// Returns the filter ID owning this pinned list (`0` = main/archive).
    #[inline]
    pub const fn filter_id(&self) -> FilterId {
        self.filter_id
    }

    /// Returns the maximum number of pinned entries.
    #[inline]
    pub const fn limit(&self) -> usize {
        self.limit
    }

    /// Returns the ordered pinned keys (index 0 corresponds to pinned index `1`).
    #[inline]
    pub fn order(&self) -> &[DialogKey] {
        &self.order
    }

    /// Returns the 1-based pinned index for `key`, or `0` if not pinned.
    pub fn pinned_index(&self, key: DialogKey) -> i32 {
        self.order
            .iter()
            .position(|&k| k == key)
            .map_or(0, |i| (i + 1) as i32)
    }

    /// Updates the maximum pin count, evicting excess tail pins and returning evicted keys.
    pub fn set_limit(&mut self, limit: usize) -> Vec<DialogKey> {
        self.limit = limit.max(1);
        if self.order.len() > self.limit {
            self.order.split_off(self.limit)
        } else {
            Vec::new()
        }
    }

    /// Appends `key` to the end of the pinned list if not already present, evicting
    /// the tail if at capacity (`PinnedList::addPinned`). Returns any evicted keys.
    pub fn add_pinned(&mut self, key: DialogKey) -> Vec<DialogKey> {
        if self.order.contains(&key) {
            return Vec::new();
        }
        let mut evicted = Vec::new();
        while self.order.len() >= self.limit {
            if let Some(removed) = self.order.pop() {
                evicted.push(removed);
            }
        }
        self.order.push(key);
        evicted
    }

    /// Pins (moving to top, index 1) or unpins `key` (`PinnedList::setPinned`).
    /// Returns any keys evicted due to capacity.
    pub fn set_pinned(&mut self, key: DialogKey, pinned: bool) -> Vec<DialogKey> {
        if pinned {
            let evicted = self.add_pinned(key);
            if let Some(pos) = self.order.iter().position(|&k| k == key)
                && pos > 0
            {
                self.order[..=pos].rotate_right(1);
            }
            evicted
        } else {
            if let Some(pos) = self.order.iter().position(|&k| k == key) {
                self.order.remove(pos);
            }
            Vec::new()
        }
    }

    /// Swaps two pinned entries (`PinnedList::reorder`). Returns `true` if both were present.
    pub fn reorder(&mut self, key1: DialogKey, key2: DialogKey) -> bool {
        let (Some(i1), Some(i2)) = (
            self.order.iter().position(|&k| k == key1),
            self.order.iter().position(|&k| k == key2),
        ) else {
            return false;
        };
        if i1 == i2 {
            return false;
        }
        self.order.swap(i1, i2);
        true
    }

    /// Replaces the entire pinned order (`PinnedList::applyList`), returning keys
    /// that were previously pinned and are no longer pinned.
    pub fn apply_list(&mut self, list: Vec<DialogKey>) -> Vec<DialogKey> {
        let old = std::mem::take(&mut self.order);
        for key in list {
            if !self.order.contains(&key) {
                self.order.push(key);
            }
        }
        old.into_iter()
            .filter(|k| !self.order.contains(k))
            .collect()
    }

    /// Clears all pinned entries and returns the previously pinned keys.
    pub fn clear(&mut self) -> Vec<DialogKey> {
        std::mem::take(&mut self.order)
    }
}

/// Entry metadata stored inside `IndexedList`.
#[derive(Clone, Debug, Eq, PartialEq)]
struct IndexedEntry {
    key: DialogKey,
    sort_key: u64,
    name_sort_key: String,
    name_words: BTreeSet<String>,
    first_letters: BTreeSet<char>,
}

/// Sorted dialog list with first-letter inverted index for fast prefix filtering
/// (`Dialogs::IndexedList` in `dialogs_indexed_list.h/.cpp`).
#[derive(Clone, Debug)]
pub struct IndexedList {
    sort_mode: SortMode,
    filter_id: FilterId,
    ordered: Vec<DialogKey>,
    entries: HashMap<DialogKey, IndexedEntry>,
    by_letter: BTreeMap<char, Vec<DialogKey>>,
}

impl IndexedList {
    /// Creates an empty `IndexedList`.
    pub fn new(sort_mode: SortMode, filter_id: FilterId) -> Self {
        Self {
            sort_mode,
            filter_id,
            ordered: Vec::new(),
            entries: HashMap::new(),
            by_letter: BTreeMap::new(),
        }
    }

    /// Returns the sort mode.
    #[inline]
    pub const fn sort_mode(&self) -> SortMode {
        self.sort_mode
    }

    /// Returns the associated filter ID (`0` = main/archive).
    #[inline]
    pub const fn filter_id(&self) -> FilterId {
        self.filter_id
    }

    /// Returns the ordered list of dialog keys.
    #[inline]
    pub fn keys(&self) -> &[DialogKey] {
        &self.ordered
    }

    /// Returns the number of entries.
    #[inline]
    pub fn len(&self) -> usize {
        self.ordered.len()
    }

    /// Returns `true` if empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.ordered.is_empty()
    }

    /// Returns `true` if `key` is in this list.
    #[inline]
    pub fn contains(&self, key: DialogKey) -> bool {
        self.entries.contains_key(&key)
    }

    /// Returns the stored sort key for `key`, if present.
    #[inline]
    pub fn sort_key(&self, key: DialogKey) -> Option<u64> {
        self.entries.get(&key).map(|e| e.sort_key)
    }

    /// Inserts or updates `key` with its sort key, name, and search tokens,
    /// maintaining exact ordering and first-letter index buckets.
    ///
    /// Moves only this entry: `ordered` stays sorted, so its old and new positions are found by
    /// binary search instead of re-sorting the whole list on every update.
    pub fn upsert(
        &mut self,
        key: DialogKey,
        sort_key: u64,
        name_sort_key: String,
        name_words: BTreeSet<String>,
        first_letters: BTreeSet<char>,
    ) {
        let Some(existing) = self.entries.get(&key) else {
            for &ch in &first_letters {
                self.by_letter.entry(ch).or_default().push(key);
            }
            let entry = IndexedEntry {
                key,
                sort_key,
                name_sort_key,
                name_words,
                first_letters,
            };
            let at = self.insert_position(&entry);
            self.ordered.insert(at, key);
            self.entries.insert(key, entry);
            return;
        };

        let moves = match self.sort_mode {
            SortMode::Date => existing.sort_key != sort_key,
            SortMode::Name => existing.name_sort_key != name_sort_key,
            SortMode::Add => false,
        };
        let from = moves.then(|| self.current_position(existing)).flatten();
        let removed_letters: Vec<char> = existing
            .first_letters
            .difference(&first_letters)
            .copied()
            .collect();
        let added_letters: Vec<char> = first_letters
            .difference(&existing.first_letters)
            .copied()
            .collect();
        if let Some(from) = from {
            self.ordered.remove(from);
        }
        for ch in removed_letters {
            if let Some(bucket) = self.by_letter.get_mut(&ch) {
                bucket.retain(|&k| k != key);
                if bucket.is_empty() {
                    self.by_letter.remove(&ch);
                }
            }
        }
        for ch in added_letters {
            self.by_letter.entry(ch).or_default().push(key);
        }

        let Some(entry) = self.entries.get_mut(&key) else {
            return;
        };
        entry.sort_key = sort_key;
        entry.name_sort_key = name_sort_key;
        entry.name_words = name_words;
        entry.first_letters = first_letters;
        if moves {
            let Some(entry) = self.entries.get(&key) else {
                return;
            };
            let at = self.insert_position(entry);
            self.ordered.insert(at, key);
        }
    }

    /// Removes `key` from the main ordering and all letter buckets.
    pub fn remove(&mut self, key: DialogKey) -> bool {
        let Some(removed) = self.entries.get(&key) else {
            return false;
        };
        if let Some(at) = self.current_position(removed) {
            self.ordered.remove(at);
        }
        let Some(removed) = self.entries.remove(&key) else {
            return true;
        };
        for ch in removed.first_letters {
            if let Some(bucket) = self.by_letter.get_mut(&ch) {
                bucket.retain(|&k| k != key);
                if bucket.is_empty() {
                    self.by_letter.remove(&ch);
                }
            }
        }
        true
    }

    /// List order of two entries: date mode by sort key descending, name mode by name
    /// ascending, ties by key so the order is total (`None` in insertion-order mode).
    fn compare(&self, a: &IndexedEntry, b: &IndexedEntry) -> Option<Ordering> {
        let primary = match self.sort_mode {
            SortMode::Date => b.sort_key.cmp(&a.sort_key),
            SortMode::Name => a.name_sort_key.cmp(&b.name_sort_key),
            SortMode::Add => return None,
        };
        Some(primary.then_with(|| a.key.cmp(&b.key)))
    }

    /// Where `entry` (not yet in `ordered`) belongs.
    fn insert_position(&self, entry: &IndexedEntry) -> usize {
        if self.sort_mode == SortMode::Add {
            return self.ordered.len();
        }
        self.ordered.partition_point(|k| {
            self.entries
                .get(k)
                .and_then(|other| self.compare(other, entry))
                .is_some_and(Ordering::is_lt)
        })
    }

    /// Index of `entry` in `ordered`, found by binary search with the values it was placed by.
    fn current_position(&self, entry: &IndexedEntry) -> Option<usize> {
        (self.sort_mode != SortMode::Add)
            .then(|| self.insert_position(entry))
            .filter(|&at| self.ordered.get(at) == Some(&entry.key))
            .or_else(|| self.ordered.iter().position(|&k| k == entry.key))
    }

    /// Clears all entries and indices.
    pub fn clear(&mut self) {
        self.ordered.clear();
        self.entries.clear();
        self.by_letter.clear();
    }

    /// Filters entries whose indexed words contain a prefix match for every word
    /// in `query`, returning results in the list's current sort order
    /// (`IndexedList::filtered(words)`).
    pub fn filter_by_query(&self, query: &str) -> Vec<DialogKey> {
        let words = split_search_words(query);
        if words.is_empty() {
            return self.ordered.clone();
        }
        // Pick the smallest letter bucket matching any query word's first char.
        let mut smallest: Option<&[DialogKey]> = None;
        for w in &words {
            let Some(ch) = w.chars().next() else {
                continue;
            };
            let Some(bucket) = self.by_letter.get(&ch) else {
                return Vec::new();
            };
            if smallest.is_none_or(|cur| bucket.len() < cur.len()) {
                smallest = Some(bucket.as_slice());
            }
        }
        let Some(candidates) = smallest else {
            return Vec::new();
        };

        let mut matched: Vec<DialogKey> = candidates
            .iter()
            .copied()
            .filter(|k| {
                let Some(entry) = self.entries.get(k) else {
                    return false;
                };
                words
                    .iter()
                    .all(|q| entry.name_words.iter().any(|w| w.starts_with(q)))
            })
            .collect();
        self.sort_slice(&mut matched);
        matched
    }

    fn sort_slice(&self, keys: &mut [DialogKey]) {
        let entries = &self.entries;
        match self.sort_mode {
            SortMode::Date => {
                keys.sort_by(|a, b| {
                    let ea = &entries[a];
                    let eb = &entries[b];
                    eb.sort_key.cmp(&ea.sort_key).then_with(|| a.cmp(b))
                });
            }
            SortMode::Name => {
                keys.sort_by(|a, b| {
                    let ea = &entries[a];
                    let eb = &entries[b];
                    ea.name_sort_key
                        .cmp(&eb.name_sort_key)
                        .then_with(|| a.cmp(b))
                });
            }
            SortMode::Add => {}
        }
    }
}

/// Complete dialog list (main list, archive list, or filter list) combining
/// `IndexedList`, `PinnedList`, local `UnreadState`, and cloud unread fallback
/// (`Dialogs::MainList` in `dialogs_main_list.h/.cpp`).
#[derive(Clone, Debug)]
pub struct MainList {
    filter_id: FilterId,
    indexed: IndexedList,
    pinned: PinnedList,
    unread_state: UnreadState,
    cloud_unread_state: UnreadState,
    entry_unread: HashMap<DialogKey, UnreadState>,
    cloud_list_size: usize,
    loaded: bool,
    all_are_muted: bool,
}

impl MainList {
    /// Creates a new `MainList` for `filter_id` (`0` = main/archive) with `pinned_limit`.
    pub fn new(filter_id: FilterId, pinned_limit: usize) -> Self {
        let unread_state = UnreadState {
            known: true,
            ..Default::default()
        };
        Self {
            filter_id,
            indexed: IndexedList::new(SortMode::Date, filter_id),
            pinned: PinnedList::new(filter_id, pinned_limit),
            unread_state,
            cloud_unread_state: UnreadState::default(),
            entry_unread: HashMap::new(),
            cloud_list_size: 0,
            loaded: false,
            all_are_muted: false,
        }
    }

    /// Returns the filter ID (`0` = main/archive).
    #[inline]
    pub const fn filter_id(&self) -> FilterId {
        self.filter_id
    }

    /// Returns `true` if no entries are currently loaded in this list.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.indexed.is_empty()
    }

    /// Returns `true` if the full dialog list has been loaded from server.
    #[inline]
    pub const fn loaded(&self) -> bool {
        self.loaded
    }

    /// Sets whether the full list is loaded (`MainList::setLoaded`).
    #[inline]
    pub fn set_loaded(&mut self, loaded: bool) {
        self.loaded = loaded;
    }

    /// Treats all entries in this list as muted for unread aggregation
    /// (used by the archive folder, `MainList::setAllAreMuted`).
    #[inline]
    pub fn set_all_are_muted(&mut self, all_are_muted: bool) {
        self.all_are_muted = all_are_muted;
    }

    /// Sets the server-reported total dialog count before the full list is loaded.
    #[inline]
    pub fn set_cloud_list_size(&mut self, size: usize) {
        self.cloud_list_size = size;
    }

    /// Returns `max(loaded_len, cloud_list_size)` when not fully loaded, else `loaded_len`.
    #[inline]
    pub fn full_size(&self) -> usize {
        if self.loaded {
            self.indexed.len()
        } else {
            self.indexed.len().max(self.cloud_list_size)
        }
    }

    /// Returns the underlying `IndexedList`.
    #[inline]
    pub fn indexed(&self) -> &IndexedList {
        &self.indexed
    }

    /// Returns the underlying `PinnedList`.
    #[inline]
    pub fn pinned(&self) -> &PinnedList {
        &self.pinned
    }

    /// Returns mutable access to the underlying `PinnedList`.
    #[inline]
    pub fn pinned_mut(&mut self) -> &mut PinnedList {
        &mut self.pinned
    }

    /// Returns `true` if cloud unread counters have been populated.
    #[inline]
    pub const fn cloud_unread_known(&self) -> bool {
        self.cloud_unread_state.known
    }

    /// Inserts or updates a dialog entry and reconciles its contribution to `unread_state`.
    pub fn upsert_entry(
        &mut self,
        key: DialogKey,
        sort_key: u64,
        name_sort_key: String,
        name_words: BTreeSet<String>,
        first_letters: BTreeSet<char>,
        unread: UnreadState,
    ) {
        let prev = self.entry_unread.insert(key, unread);
        match prev {
            Some(was) => self.unread_state_changed(was, unread),
            None => self.unread_entry_changed(unread, true),
        }
        self.indexed
            .upsert(key, sort_key, name_sort_key, name_words, first_letters);
    }

    /// Removes a dialog entry and subtracts its unread contribution.
    pub fn remove_entry(&mut self, key: DialogKey) -> bool {
        let removed = self.indexed.remove(key);
        if let Some(was) = self.entry_unread.remove(&key) {
            self.unread_entry_changed(was, false);
        }
        removed
    }

    /// Updates a single entry's unread contribution (`MainList::unreadStateChanged`).
    pub fn unread_state_changed(&mut self, was_state: UnreadState, now_state: UnreadState) {
        let delta = now_state - was_state;
        self.unread_state += delta;
        if self.cloud_unread_state.known && was_state.known && now_state.known {
            self.cloud_unread_state += delta;
            self.finalize_cloud_unread();
        }
    }

    /// Adds or removes an entry's unread contribution (`MainList::unreadEntryChanged`).
    fn unread_entry_changed(&mut self, state: UnreadState, added: bool) {
        if state.messages == 0
            && state.chats == 0
            && state.marks == 0
            && state.mentions == 0
            && state.reactions == 0
            && state.polls == 0
        {
            return;
        }
        if added {
            self.unread_state += state;
        } else {
            self.unread_state -= state;
        }
        if self.cloud_unread_state.known && state.known {
            if added {
                self.cloud_unread_state += state;
            } else {
                self.cloud_unread_state -= state;
            }
            self.finalize_cloud_unread();
        }
    }

    /// Applies cloud unread totals from `dialogFolder` (`MainList::updateCloudUnread`).
    pub fn update_cloud_unread(
        &mut self,
        unread_muted_messages: i32,
        unread_unmuted_messages: i32,
        unread_muted_peers: i32,
        unread_unmuted_peers: i32,
    ) {
        self.cloud_unread_state.messages =
            unread_muted_messages.saturating_add(unread_unmuted_messages);
        self.cloud_unread_state.chats = unread_muted_peers.saturating_add(unread_unmuted_peers);
        self.finalize_cloud_unread();
        self.cloud_unread_state.known = true;
    }

    fn finalize_cloud_unread(&mut self) {
        // Cloud state for archive folder always counts everything as muted.
        self.cloud_unread_state.messages_muted = self.cloud_unread_state.messages;
        self.cloud_unread_state.chats_muted = self.cloud_unread_state.chats;
        self.cloud_unread_state.marks = 0;
        self.cloud_unread_state.marks_muted = 0;
    }

    /// Computes effective `UnreadState` for this list (`MainList::unreadState`).
    pub fn unread_state(&self) -> UnreadState {
        let use_cloud = self.cloud_unread_state.known && !self.loaded;
        let mut result = if use_cloud {
            self.cloud_unread_state
        } else {
            self.unread_state
        };
        if use_cloud {
            result.marks = self.unread_state.marks;
            result.marks_muted = self.unread_state.marks_muted;
        }
        if self.all_are_muted {
            result.messages_muted = result.messages;
            result.chats_muted = result.chats;
            result.marks_muted = result.marks;
        }
        result
    }

    /// Clears all entries, pins, and counters (`MainList::clear`).
    pub fn clear(&mut self) {
        self.pinned.clear();
        self.indexed.clear();
        self.entry_unread.clear();
        self.unread_state = UnreadState {
            known: true,
            ..Default::default()
        };
        self.cloud_unread_state = UnreadState {
            known: true,
            ..Default::default()
        };
        self.cloud_list_size = 0;
    }
}

/// Dialog folder model (primarily the Archive folder, `FolderId = 1`)
/// (`Data::Folder` in `data_folder.h/.cpp`).
#[derive(Clone, Debug)]
pub struct Folder {
    /// Folder ID (`ARCHIVE_FOLDER_ID = 1`).
    pub id: FolderId,
    /// Display name (e.g. `"Archived Chats"`).
    pub name: String,
    /// Dialog list of chats inside this folder (`all_are_muted = true`).
    pub chats_list: MainList,
    /// Whether the archive entry is pinned/fixed at the top of the main chats list.
    pub fixed_on_top: bool,
    /// Number of active stories from archived contacts.
    pub stories_count: u16,
    /// Number of unread stories from archived contacts.
    pub stories_unread_count: u16,
}

impl Folder {
    /// Creates a new `Folder` (defaults to archive folder settings).
    pub fn new(id: FolderId, pinned_limit: usize) -> Self {
        let mut chats_list = MainList::new(0, pinned_limit);
        chats_list.set_all_are_muted(true);
        Self {
            id,
            name: if id == ARCHIVE_FOLDER_ID {
                "Archived Chats".to_owned()
            } else {
                format!("Folder {id}")
            },
            chats_list,
            fixed_on_top: true,
            stories_count: 0,
            stories_unread_count: 0,
        }
    }

    /// Returns `ARCHIVE_FIX_ON_TOP_INDEX` if fixed on top, else `0`.
    #[inline]
    pub const fn fixed_on_top_index(&self) -> i32 {
        if self.fixed_on_top {
            ARCHIVE_FIX_ON_TOP_INDEX
        } else {
            0
        }
    }

    /// Returns `true` if this folder should be shown in the main chats list.
    pub fn should_be_in_chat_list(&self) -> bool {
        !self.chats_list.is_empty() || self.stories_count > 0
    }

    /// Returns the aggregated `UnreadState` of all dialogs in this folder.
    pub fn unread_state(&self) -> UnreadState {
        self.chats_list.unread_state()
    }

    /// Returns the badge state for the archive row.
    pub fn badges_state(&self, settings: BadgeSettings) -> BadgesState {
        badges_for_unread(
            &self.unread_state(),
            CountInBadge::Chats,
            IncludeInBadge::All,
            settings,
        )
    }
}

bitflags! {
    /// Chat filter (folder) rule and metadata flags (`Data::ChatFilter::Flag`
    /// in `data_chat_filters.h`).
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
    pub struct ChatFilterFlags: u16 {
        /// Include contacts.
        const CONTACTS       = 1 << 0;
        /// Include non-contact users.
        const NON_CONTACTS   = 1 << 1;
        /// Include basic groups and megagroups.
        const GROUPS         = 1 << 2;
        /// Include broadcast channels.
        const CHANNELS       = 1 << 3;
        /// Include bots.
        const BOTS           = 1 << 4;
        /// Exclude muted chats (unless unread mention in unarchived chat).
        const NO_MUTED       = 1 << 5;
        /// Exclude read chats.
        const NO_READ        = 1 << 6;
        /// Exclude archived chats.
        const NO_ARCHIVED    = 1 << 7;
        /// Mask covering all inclusion/exclusion rule flags.
        const RULES_MASK     = (1 << 8) - 1;

        /// Shareable folder (chatlist).
        const CHATLIST       = 1 << 8;
        /// Folder has invite links created by the current user.
        const HAS_MY_LINKS   = 1 << 9;
        /// Custom emoji in folder title are rendered statically.
        const STATIC_TITLE   = 1 << 10;

        /// Telegram Business exception: new chats.
        const NEW_CHATS      = 1 << 11;
        /// Telegram Business exception: existing chats.
        const EXISTING_CHATS = 1 << 12;
    }
}

/// Custom chat filter (folder) definition (`Data::ChatFilter` in `data_chat_filters.h/.cpp`).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ChatFilter {
    /// Server-assigned filter ID (`0` = default "All Chats").
    pub id: FilterId,
    /// Folder title with optional custom emoji entities.
    pub title: TextWithEntities,
    /// Emoticon icon string (e.g. `"💬"`).
    pub icon_emoji: String,
    /// Optional folder tag color index (`0..7`).
    pub color_index: Option<u8>,
    /// Inclusion, exclusion, and chatlist flags.
    pub flags: ChatFilterFlags,
    /// Peers always included (superset of `pinned` in tdesktop).
    pub always: BTreeSet<PeerId>,
    /// Ordered peers pinned inside this filter.
    pub pinned: Vec<PeerId>,
    /// Peers explicitly excluded (overrides rules and `always`).
    pub never: BTreeSet<PeerId>,
}

impl ChatFilter {
    /// Creates a new `ChatFilter`, automatically unioning `pinned` into `always`
    /// just like `ChatFilter::FromTL`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: FilterId,
        title: TextWithEntities,
        icon_emoji: impl Into<String>,
        color_index: Option<u8>,
        flags: ChatFilterFlags,
        mut always: BTreeSet<PeerId>,
        pinned: Vec<PeerId>,
        never: BTreeSet<PeerId>,
    ) -> Self {
        for &peer_id in &pinned {
            always.insert(peer_id);
        }
        Self {
            id,
            title,
            icon_emoji: icon_emoji.into(),
            color_index,
            flags,
            always,
            pinned,
            never,
        }
    }

    /// Returns `true` if `history` (with its `peer`) matches this filter
    /// (`ChatFilter::contains` in `data_chat_filters.cpp`).
    pub fn contains(&self, peer: &Peer, history: &History, ignore_fake_unread: bool) -> bool {
        let peer_id = history.peer_id;
        if self.never.contains(&peer_id) {
            return false;
        }
        if let Some(channel) = peer.as_channel()
            && channel.is_community()
        {
            return self.always.contains(&peer_id);
        }
        let type_flag = match peer {
            Peer::User(user) => {
                if user.is_bot() {
                    ChatFilterFlags::BOTS
                } else if user.is_contact() {
                    ChatFilterFlags::CONTACTS
                } else {
                    ChatFilterFlags::NON_CONTACTS
                }
            }
            Peer::Chat(_) => ChatFilterFlags::GROUPS,
            Peer::Channel(channel) => {
                if channel.is_broadcast() {
                    ChatFilterFlags::CHANNELS
                } else {
                    ChatFilterFlags::GROUPS
                }
            }
        };

        let state = if self
            .flags
            .intersects(ChatFilterFlags::NO_MUTED | ChatFilterFlags::NO_READ)
        {
            history.badges_state()
        } else {
            BadgesState::default()
        };

        let matches_rules = self.flags.contains(type_flag)
            && (!self.flags.contains(ChatFilterFlags::NO_MUTED)
                || !history.muted
                || (state.mention && history.folder_known() && !history.is_archived()))
            && (!self.flags.contains(ChatFilterFlags::NO_READ)
                || state.unread
                || state.mention
                || (!ignore_fake_unread && history.fake_unread_while_opened))
            && (!self.flags.contains(ChatFilterFlags::NO_ARCHIVED)
                || (history.folder_known() && !history.is_archived()));

        matches_rules || self.always.contains(&peer_id)
    }

    /// Returns `true` if this filter is a shareable chatlist.
    #[inline]
    pub const fn is_chatlist(&self) -> bool {
        self.flags.contains(ChatFilterFlags::CHATLIST)
    }

    /// Returns `true` if custom emoji in the title should not animate.
    #[inline]
    pub const fn static_title(&self) -> bool {
        self.flags.contains(ChatFilterFlags::STATIC_TITLE)
    }
}

/// Manages custom chat filters and their per-filter `MainList`s
/// (`Data::ChatFilters`).
#[derive(Clone, Debug)]
pub struct ChatFilters {
    list: Vec<ChatFilter>,
    chats_lists: HashMap<FilterId, MainList>,
    default_pinned_limit: usize,
    tags_enabled: bool,
}

impl Default for ChatFilters {
    fn default() -> Self {
        Self::new(100)
    }
}

impl ChatFilters {
    /// Creates a new `ChatFilters` container with default pinned limit per filter.
    pub fn new(default_pinned_limit: usize) -> Self {
        Self {
            list: Vec::new(),
            chats_lists: HashMap::new(),
            default_pinned_limit: default_pinned_limit.max(1),
            tags_enabled: false,
        }
    }

    /// Returns the ordered list of custom filters.
    pub fn list(&self) -> &[ChatFilter] {
        &self.list
    }

    /// Finds a filter by ID.
    pub fn get(&self, id: FilterId) -> Option<&ChatFilter> {
        self.list.iter().find(|f| f.id == id)
    }

    /// Returns whether colored folder tags are enabled.
    #[inline]
    pub const fn tags_enabled(&self) -> bool {
        self.tags_enabled
    }

    /// Sets whether colored folder tags are enabled.
    #[inline]
    pub fn set_tags_enabled(&mut self, enabled: bool) {
        self.tags_enabled = enabled;
    }

    /// Inserts or replaces a filter and ensures its `MainList` exists.
    ///
    /// Returns keys that were previously pinned in this filter and are no longer pinned
    /// (`data/data_chat_filters.cpp:780-782`, `dialogs/dialogs_pinned_list.cpp:146-165`).
    pub fn upsert(&mut self, filter: ChatFilter) -> Vec<DialogKey> {
        let id = filter.id;
        let pinned_keys: Vec<DialogKey> = filter
            .pinned
            .iter()
            .copied()
            .map(DialogKey::History)
            .collect();
        if let Some(existing) = self.list.iter_mut().find(|f| f.id == id) {
            *existing = filter;
        } else {
            self.list.push(filter);
        }
        let list = self
            .chats_lists
            .entry(id)
            .or_insert_with(|| MainList::new(id, self.default_pinned_limit));
        list.pinned_mut().apply_list(pinned_keys)
    }

    /// Removes a filter and its `MainList`.
    pub fn remove(&mut self, id: FilterId) -> bool {
        let before = self.list.len();
        self.list.retain(|f| f.id != id);
        self.chats_lists.remove(&id);
        self.list.len() != before
    }

    /// Reorders filters to match `order` (unknown IDs ignored; unlisted filters kept at end).
    pub fn apply_order(&mut self, order: &[FilterId]) {
        let mut reordered = Vec::with_capacity(self.list.len());
        for &id in order {
            if let Some(pos) = self.list.iter().position(|f| f.id == id) {
                reordered.push(self.list.remove(pos));
            }
        }
        reordered.append(&mut self.list);
        self.list = reordered;
    }

    /// Reorders filters strictly matching `ChatFilters::applyOrder`
    /// (`data/data_chat_filters.cpp:799-835`): ignores `0` (the default "All Chats" tab) and
    /// returns `false` if the remaining IDs do not form a bijection with `self.list`,
    /// signalling that the caller must reload via `messages.getDialogFilters`.
    pub fn apply_order_checked(&mut self, order: &[FilterId]) -> bool {
        let non_default: Vec<FilterId> = order.iter().copied().filter(|&id| id != 0).collect();
        if non_default.len() != self.list.len() {
            return false;
        }
        if self.list.is_empty() {
            return true;
        }
        let mut seen = BTreeSet::new();
        for &id in &non_default {
            if !seen.insert(id) || !self.list.iter().any(|f| f.id == id) {
                return false;
            }
        }
        self.apply_order(&non_default);
        true
    }

    /// Returns the `MainList` for `filter_id`, if present.
    pub fn chats_list(&self, filter_id: FilterId) -> Option<&MainList> {
        self.chats_lists.get(&filter_id)
    }

    /// Returns mutable access to the `MainList` for `filter_id`, if present.
    pub fn chats_list_mut(&mut self, filter_id: FilterId) -> Option<&mut MainList> {
        self.chats_lists.get_mut(&filter_id)
    }

    /// Computes the unread badge for `filter_id` (`BadgesForUnread` with
    /// `CountInBadge::Default` and `IncludeInBadge::UnmutedOrAll` — matching
    /// tdesktop's filter tab badge rules).
    pub fn unread_badge(
        &self,
        filter_id: FilterId,
        settings: BadgeSettings,
    ) -> Option<BadgesState> {
        let list = self.chats_lists.get(&filter_id)?;
        Some(badges_for_unread(
            &list.unread_state(),
            CountInBadge::Default,
            IncludeInBadge::UnmutedOrAll,
            settings,
        ))
    }

    /// Computes the unread badge for `filter_id` with explicit count/include modes.
    pub fn unread_badge_with(
        &self,
        filter_id: FilterId,
        count: CountInBadge,
        include: IncludeInBadge,
        settings: BadgeSettings,
    ) -> Option<BadgesState> {
        let list = self.chats_lists.get(&filter_id)?;
        Some(badges_for_unread(
            &list.unread_state(),
            count,
            include,
            settings,
        ))
    }

    /// Returns `true` if any filter can match archived chats (`ChatFilters::archiveNeeded`).
    pub fn archive_needed(&self) -> bool {
        self.list
            .iter()
            .any(|f| !f.flags.contains(ChatFilterFlags::NO_ARCHIVED) || !f.always.is_empty())
    }
}
