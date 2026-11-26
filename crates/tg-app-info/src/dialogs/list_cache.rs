//! What the chat list keeps between frames, so that a frame without changes converts no chat,
//! measures no text and sums no row heights.
//!
//! - [`ListSnapshot`]: the rows of the shown list as the backend returned them, with their
//!   heights and running tops. It is read again only when
//!   [`DialogsBackend::dialogs_version`] moves, the request changes, or the snapshot is older
//!   than [`SNAPSHOT_MAX_AGE_SECS`]. A backend that does not count its changes returns a new
//!   version on every call and is read every frame, as before.
//! - [`RowTextCache`]: the prepared text and geometry of the rows that are painted, rebuilt for a
//!   row only when that row, its width or the fonts change.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ops::Range;
use std::sync::Arc;

use egui::Painter;
use tg_history::datetime::CivilDate;
use tg_history::{DialogsDateKind, LocalDateTime, classify_dialogs_date};

use crate::dialogs::backend::{
    ArchiveSummary, DialogItem, DialogListQuery, DialogSlice, DialogsBackend,
};
use crate::dialogs::layout::{
    PreparedDialogRow, RowPaintState, TextEpoch, prepare_dialog_row, row_style_for, text_epoch,
};
use tg_app_core::stand_in::types::{FilterId, PeerId};

/// Longest time a snapshot is shown without asking the backend again.
///
/// Some of what a row shows changes with the clock alone (a user going offline, a mute running
/// out), and a live session continues loading the chat list from these reads, so the list is
/// read at least this often while frames are painted.
pub const SNAPSHOT_MAX_AGE_SECS: f64 = 1.0;

/// [`SNAPSHOT_MAX_AGE_SECS`] while a row shows a send action ("typing"): that state ends on a
/// timer of a few seconds, and its end should not wait for the next second.
pub const TYPING_SNAPSHOT_MAX_AGE_SECS: f64 = 0.25;

/// Prepared rows kept before the ones that are no longer painted are dropped.
const ROW_TEXT_CACHE_LIMIT: usize = 256;

/// Counters of the work the chat list did since it was created. A frame that changes neither
/// counter converted no chat and prepared no row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DialogsFrameStats {
    /// Times the shown list was read from the backend (each read converts every loaded row).
    pub list_reads: u64,
    /// Rows whose text and geometry were prepared.
    pub rows_prepared: u64,
}

/// Everything a list read depends on besides the backend's own state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ListRequest {
    pub(crate) list: DialogListQuery,
    pub(crate) loaded_limit: usize,
    pub(crate) show_folder_tags: bool,
    pub(crate) narrow: bool,
}

/// One read of the shown list with the per-row geometry derived from it.
#[derive(Debug)]
pub(crate) struct ListSnapshot {
    request: ListRequest,
    version: u64,
    read_at: f64,
    /// Loaded rows (community rows already grouped) and the list totals.
    pub(crate) slice: DialogSlice,
    /// Archive row above "All Chats", if it is shown.
    pub(crate) archive_summary: Option<ArchiveSummary>,
    /// The first page of the main list is still on its way.
    pub(crate) chats_loading: bool,
    /// Height of each row of `slice.entries`.
    pub(crate) row_heights: Vec<f32>,
    /// Top of each row measured from the top of the first one, then the bottom of the last row.
    row_tops: Vec<f32>,
    /// A row shows a send action, so the dots keep animating.
    pub(crate) any_typing: bool,
}

impl ListSnapshot {
    fn read(backend: &dyn DialogsBackend, request: ListRequest, version: u64, time: f64) -> Self {
        let all_chats = matches!(request.list, DialogListQuery::Filter(FilterId::ALL));
        let archive_summary = if all_chats {
            backend.archive_summary()
        } else {
            None
        };
        let mut slice = backend.dialog_slice(request.list, 0, request.loaded_limit);
        let chats_loading = all_chats && slice.entries.is_empty() && !backend.chats_list_loaded();
        if all_chats {
            let groups = backend.community_groups();
            if groups.iter().any(|g| g.collapsed_in_dialogs) {
                let grouped = crate::dialogs::community::group_dialogs_with_communities(
                    &slice.entries,
                    &groups,
                );
                slice.entries = grouped
                    .into_iter()
                    .map(|row| row.as_dialog_item().clone())
                    .collect();
                slice.pinned_count = slice.entries.iter().take_while(|d| d.pinned).count();
            }
        }
        let row_heights: Vec<f32> = slice
            .entries
            .iter()
            .map(|item| row_style_for(item, request.show_folder_tags, request.narrow).height as f32)
            .collect();
        let row_tops = running_row_tops(&row_heights);
        let any_typing = slice.entries.iter().any(|d| d.typing.is_some());
        Self {
            request,
            version,
            read_at: time,
            slice,
            archive_summary,
            chats_loading,
            row_heights,
            row_tops,
            any_typing,
        }
    }

    /// Whether this snapshot still answers `request` at `version` and `time`.
    fn serves(&self, request: ListRequest, version: u64, time: f64) -> bool {
        // An empty list costs nothing to read, and it is where the first page of chats shows up.
        if self.slice.entries.is_empty() {
            return false;
        }
        let max_age = if self.any_typing {
            TYPING_SNAPSHOT_MAX_AGE_SECS
        } else {
            SNAPSHOT_MAX_AGE_SECS
        };
        let age = time - self.read_at;
        self.request == request && self.version == version && (0.0..max_age).contains(&age)
    }

    /// Height of all loaded rows.
    pub(crate) fn rows_height(&self) -> f32 {
        self.row_tops.last().copied().unwrap_or(0.0)
    }

    /// Rows intersecting `[viewport_top, viewport_bottom)` and the top of the first of them,
    /// both measured from the top of the first row.
    pub(crate) fn visible_rows(
        &self,
        viewport_top: f32,
        viewport_bottom: f32,
    ) -> (Range<usize>, f32) {
        visible_rows_by_tops(viewport_top, viewport_bottom, &self.row_tops)
    }
}

/// Running tops of rows with `heights`: `heights.len() + 1` values starting at `0.0`, the last
/// one being the total height.
pub(crate) fn running_row_tops(heights: &[f32]) -> Vec<f32> {
    let mut tops = Vec::with_capacity(heights.len() + 1);
    let mut y = 0.0_f32;
    tops.push(y);
    for &h in heights {
        y += h;
        tops.push(y);
    }
    tops
}

/// [`crate::dialogs::inner::variable_visible_row_range`] over the running tops from
/// [`running_row_tops`]: the same range and start offset, found by two binary searches instead
/// of a walk from the first row.
pub(crate) fn visible_rows_by_tops(
    viewport_top: f32,
    viewport_bottom: f32,
    row_tops: &[f32],
) -> (Range<usize>, f32) {
    let rows = row_tops.len().saturating_sub(1);
    if rows == 0 || viewport_bottom <= viewport_top {
        return (0..0, 0.0);
    }
    // First row whose bottom is below the viewport top.
    let from = row_tops[1..].partition_point(|&bottom| bottom <= viewport_top);
    // First row whose top is at or below the viewport bottom.
    let to = row_tops[..rows].partition_point(|&top| top < viewport_bottom);
    if from >= to {
        (0..0, 0.0)
    } else {
        (from..to, row_tops[from])
    }
}

/// The snapshot of the shown list, read from the backend only when it can have changed.
#[derive(Debug, Default)]
pub(crate) struct ListCache {
    snapshot: Option<Arc<ListSnapshot>>,
    reads: u64,
}

impl ListCache {
    /// Returns the rows for `request`, reading them from `backend` unless the kept snapshot
    /// still serves. `time` is the frame clock in seconds.
    pub(crate) fn snapshot(
        &mut self,
        backend: &dyn DialogsBackend,
        request: ListRequest,
        time: f64,
    ) -> Arc<ListSnapshot> {
        let version = backend.dialogs_version();
        if let Some(snapshot) = &self.snapshot
            && snapshot.serves(request, version, time)
        {
            return Arc::clone(snapshot);
        }
        self.reads += 1;
        let snapshot = Arc::new(ListSnapshot::read(backend, request, version, time));
        self.snapshot = Some(Arc::clone(&snapshot));
        snapshot
    }

    /// Drops the snapshot: the list changed through this widget.
    pub(crate) fn invalidate(&mut self) {
        self.snapshot = None;
    }

    /// Times the list was read from the backend.
    pub(crate) const fn reads(&self) -> u64 {
        self.reads
    }
}

/// One backend read (the folder list) kept while [`DialogsBackend::dialogs_version`] stays the
/// same and the read is younger than [`SNAPSHOT_MAX_AGE_SECS`].
#[derive(Debug)]
pub(crate) struct VersionedRead<T> {
    kept: Option<(u64, f64, Arc<T>)>,
    reads: u64,
}

impl<T> Default for VersionedRead<T> {
    fn default() -> Self {
        Self {
            kept: None,
            reads: 0,
        }
    }
}

impl<T> VersionedRead<T> {
    /// The kept value if it was read at `version` less than a snapshot age before `time`,
    /// otherwise the result of `read`, which is kept from now on.
    pub(crate) fn get(&mut self, version: u64, time: f64, read: impl FnOnce() -> T) -> Arc<T> {
        if let Some((kept_version, read_at, value)) = &self.kept
            && *kept_version == version
            && (0.0..SNAPSHOT_MAX_AGE_SECS).contains(&(time - read_at))
        {
            return Arc::clone(value);
        }
        self.reads += 1;
        let value = Arc::new(read());
        self.kept = Some((version, time, Arc::clone(&value)));
        value
    }

    /// Drops the kept value: what it was read from changed through its owner.
    pub(crate) fn invalidate(&mut self) {
        self.kept = None;
    }

    /// Times the value was read.
    pub(crate) const fn reads(&self) -> u64 {
        self.reads
    }
}

/// What the prepared text of a row depends on besides the row itself and the fonts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RowTextKey {
    width: i32,
    narrow: bool,
    show_folder_tags: bool,
    use_24h: bool,
    /// How the row's date is written, and for a full date the day it is written on.
    date: Option<(DialogsDateKind, Option<CivilDate>)>,
}

impl RowTextKey {
    fn new(item: &DialogItem, now: LocalDateTime, width: f32, state: RowPaintState) -> Self {
        let has_date = item.last_message_id.0 != 0 || item.effective_draft().is_some();
        let date = has_date.then(|| {
            let kind = classify_dialogs_date(item.display_time(), now);
            // Only a full date reads `now` again ("12.03" this year, "12.03.24" otherwise).
            (
                kind,
                matches!(kind, DialogsDateKind::Date).then_some(now.date),
            )
        });
        Self {
            width: width.round() as i32,
            narrow: state.narrow,
            show_folder_tags: state.show_folder_tags,
            use_24h: state.use_24h,
            date,
        }
    }
}

#[derive(Debug)]
struct CachedRowText {
    key: RowTextKey,
    /// The row this text was prepared from.
    item: DialogItem,
    used_frame: u64,
    prepared: PreparedDialogRow,
}

/// Prepared text of the painted rows, by peer.
#[derive(Debug, Default)]
pub(crate) struct RowTextCache {
    rows: HashMap<PeerId, CachedRowText>,
    epoch: Option<TextEpoch>,
    frame: u64,
    prepared: u64,
}

impl RowTextCache {
    /// Call once per frame before [`Self::row`]: forgets everything when the fonts, the scale
    /// or the language changed, and drops rows that are no longer painted.
    pub(crate) fn begin_frame(&mut self, painter: &Painter) {
        let epoch = text_epoch(painter);
        if self.epoch != Some(epoch) {
            self.rows.clear();
            self.epoch = Some(epoch);
        }
        self.frame = self.frame.wrapping_add(1);
        if self.rows.len() > ROW_TEXT_CACHE_LIMIT {
            let frame = self.frame;
            self.rows
                .retain(|_, row| frame.wrapping_sub(row.used_frame) <= 1);
        }
    }

    /// Prepared text of `item` at `width`, built now only if the row or its width changed.
    pub(crate) fn row(
        &mut self,
        painter: &Painter,
        item: &DialogItem,
        now: LocalDateTime,
        width: f32,
        state: RowPaintState,
    ) -> &PreparedDialogRow {
        let key = RowTextKey::new(item, now, width, state);
        let frame = self.frame;
        match self.rows.entry(item.peer_id) {
            Entry::Occupied(entry) => {
                let row = entry.into_mut();
                if row.key != key || row.item != *item {
                    self.prepared += 1;
                    row.prepared = prepare_dialog_row(painter, item, now, width, state);
                    row.item = item.clone();
                    row.key = key;
                }
                row.used_frame = frame;
                &row.prepared
            }
            Entry::Vacant(entry) => {
                self.prepared += 1;
                &entry
                    .insert(CachedRowText {
                        key,
                        item: item.clone(),
                        used_frame: frame,
                        prepared: prepare_dialog_row(painter, item, now, width, state),
                    })
                    .prepared
            }
        }
    }

    /// Rows prepared so far.
    pub(crate) const fn prepared(&self) -> u64 {
        self.prepared
    }
}
