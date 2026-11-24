//! Port of `dialogs/ui/dialogs_stories_list.h`, `dialogs/ui/dialogs_stories_list.cpp`,
//! `dialogs/ui/dialogs_stories_content.h`, and `dialogs/ui/dialogs_stories_content.cpp`.
//!
//! Pure-logic data model for the "stories in dialogs list" top bar:
//! - `DialogStoriesContent` and `DialogStoryElement` (total count, unread count, live stream flag,
//!   self/mine unread highlight, upload progress / failure state)
//! - Userpic outline ring segment builder (`compute_ring_segments` matching `List::validateSegments`
//!   and the `mine && mine_unread` last-segment highlight)
//! - Collapsed (small) vs expanded (full) bar state machine with exact hysteresis ratios,
//!   collapsed thumbnail selection (`kSmallThumbsShown = 3`, `skipSmall` for self),
//!   tooltip summary names (`kMaxTooltipNames = 3`), and horizontal scroll load-more triggers.

use tg_data::{PeerId, StoryId};

use crate::store::{MAX_SEGMENTS_COUNT, StoriesStore, StorySourcesList};

// ---------------------------------------------------------------------------
// Constants matching dialogs/ui/dialogs_stories_list.cpp & dialogs_stories_content.cpp
// ---------------------------------------------------------------------------

/// Maximum userpics displayed in the collapsed/small header strip (`kSmallThumbsShown`).
pub const SMALL_THUMBS_SHOWN: usize = 3;

/// Maximum recent stories shown in peer profile top bar (`kShownLastCount`).
pub const SHOWN_LAST_COUNT: usize = 3;

/// Horizontal scroll preload threshold in viewport widths (`kPreloadPages`).
pub const PRELOAD_PAGES: i32 = 2;

/// Expand ratio threshold to transition from collapsed to expanded (`kExpandAfterRatio`).
pub const EXPAND_AFTER_RATIO: f64 = 0.72;

/// Collapse ratio threshold to transition from expanded to collapsed (`kCollapseAfterRatio`).
pub const COLLAPSE_AFTER_RATIO: f64 = 0.68;

/// Friction ratio applied while pulling from collapsed state (`kFrictionRatio`).
pub const FRICTION_RATIO: f64 = 0.15;

/// Maximum peer names listed in the collapsed strip tooltip (`kMaxTooltipNames`).
pub const MAX_TOOLTIP_NAMES: usize = 3;

// ---------------------------------------------------------------------------
// Content & Upload Models (`Dialogs::Stories::Element`, `Content`)
// ---------------------------------------------------------------------------

/// Active outgoing story upload status merged into the dialogs stories strip.
#[derive(Clone, Debug, PartialEq)]
pub struct StoryUploadStatus {
    /// Target peer ID (self or channel).
    pub peer_id: PeerId,
    /// Display short name for the peer.
    pub peer_name: String,
    /// Normalized upload progress in `[0.0, 1.0]`.
    pub progress: f64,
    /// Non-empty if the upload failed.
    pub failed: bool,
}

/// A single item in the dialogs-list stories top bar (`Dialogs::Stories::Element`).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DialogStoryElement {
    /// Peer ID (or `StoryId` encoded as `u64` for `build_last_for_peer`).
    pub id: u64,
    /// Short peer name displayed beneath the userpic in expanded mode.
    pub name: String,
    /// Total active story count (clamped to `MAX_SEGMENTS_COUNT` = 180).
    pub count: u32,
    /// Unread active story count (clamped to `MAX_SEGMENTS_COUNT` = 180).
    pub unread_count: u32,
    /// Whether this peer currently has a live video stream story.
    pub has_video_stream: bool,
    /// Whether this item is skipped in the collapsed/small strip (true for self when other peers exist).
    pub skip_small: bool,
    /// Whether this item belongs to the current logged-in user.
    pub mine: bool,
    /// Whether the self user has a newly posted story beyond `viewed_self_till` (highlights last segment).
    pub mine_unread: bool,
    /// Whether a story upload is in progress for this peer.
    pub uploading: bool,
    /// Whether the active story upload failed.
    pub upload_failed: bool,
    /// Integer upload progress percentage in `0..=100`.
    pub upload_progress: u8,
}

impl DialogStoryElement {
    /// Returns `true` if this element should render with an active/unread emphasis in collapsed mode.
    #[inline]
    pub const fn is_unread_in_small(&self) -> bool {
        self.has_video_stream || self.unread_count > 0 || self.mine_unread
    }

    /// Returns `true` if the userpic ring has a read (muted) portion.
    #[inline]
    pub const fn has_read_segments(&self) -> bool {
        let effective_unread = if self.has_video_stream {
            1
        } else {
            self.unread_count
        };
        effective_unread < self.count
    }
}

/// Complete snapshot of the stories strip (`Dialogs::Stories::Content`).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DialogStoriesContent {
    /// Ordered elements ready for rendering.
    pub elements: Vec<DialogStoryElement>,
    /// Total sources count (displayed in the collapsed title, e.g. `"5 stories"`).
    pub total: usize,
}

impl DialogStoriesContent {
    /// Returns `true` if there are no story elements to display.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }
}

/// Builds `DialogStoriesContent` from `store` for `list` (`ContentForSession` / `State::next`
/// in `dialogs_stories_content.cpp:69-151`).
pub fn build_dialog_stories_content(
    store: &StoriesStore,
    list: StorySourcesList,
    mut peer_short_name: impl FnMut(PeerId) -> String,
    uploads: &[StoryUploadStatus],
) -> DialogStoriesContent {
    let sources = store.sources(list);
    let self_id = store.self_peer_id();
    let viewed_self_till = store.viewed_self_till();
    let mut elements = Vec::with_capacity(sources.len() + uploads.len());

    for info in sources {
        let Some(source) = store.source(info.id) else {
            continue;
        };
        let is_self = info.id == self_id;
        let mine_unread = is_self
            && source
                .ids
                .iter()
                .next_back()
                .is_some_and(|last| last.id > viewed_self_till);
        elements.push(DialogStoryElement {
            id: info.id.raw(),
            name: peer_short_name(info.id),
            count: info.count.min(MAX_SEGMENTS_COUNT),
            unread_count: info.unread_count.min(MAX_SEGMENTS_COUNT),
            has_video_stream: info.has_video_stream,
            skip_small: is_self,
            mine: is_self,
            mine_unread,
            uploading: false,
            upload_failed: false,
            upload_progress: 0,
        });
    }

    let mut total = elements.len();
    if list == StorySourcesList::NotHidden {
        for upload in uploads {
            let id = upload.peer_id.raw();
            let progress = (upload.progress * 100.0).round().clamp(0.0, 100.0) as u8;
            if let Some(existing) = elements.iter_mut().find(|e| e.id == id) {
                existing.uploading = true;
                existing.mine_unread = true;
                existing.upload_failed = upload.failed;
                existing.upload_progress = progress;
            } else {
                let is_self = upload.peer_id == self_id;
                elements.insert(
                    0,
                    DialogStoryElement {
                        id,
                        name: upload.peer_name.clone(),
                        count: 0,
                        unread_count: 0,
                        has_video_stream: false,
                        skip_small: is_self,
                        mine: is_self,
                        mine_unread: true,
                        uploading: true,
                        upload_failed: upload.failed,
                        upload_progress: progress,
                    },
                );
                total += 1;
            }
        }
    }

    DialogStoriesContent { elements, total }
}

/// Builds the top-bar mini-strip of up to `SHOWN_LAST_COUNT` (3) most recent active stories
/// for a single peer (`LastForPeer` in `dialogs_stories_content.cpp:175-280`).
pub fn build_last_for_peer(store: &StoriesStore, peer_id: PeerId) -> DialogStoriesContent {
    let Some(source) = store.source(peer_id) else {
        return DialogStoriesContent::default();
    };
    let read_till = source.read_till;
    let total = source.ids.len();
    let elements = source
        .ids
        .iter()
        .rev()
        .take(SHOWN_LAST_COUNT)
        .map(|entry| {
            let unread = entry.video_stream || entry.id > read_till;
            DialogStoryElement {
                id: entry.id as u64,
                name: String::new(),
                count: 1,
                unread_count: if unread { 1 } else { 0 },
                has_video_stream: entry.video_stream,
                skip_small: false,
                mine: false,
                mine_unread: false,
                uploading: false,
                upload_failed: false,
                upload_progress: 0,
            }
        })
        .collect();
    DialogStoriesContent { elements, total }
}

// ---------------------------------------------------------------------------
// Userpic Ring Segments (`List::validateSegments` in `dialogs_stories_list.cpp`)
// ---------------------------------------------------------------------------

/// Visual style of a single segment around a peer's userpic ring.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum RingSegmentKind {
    /// Already read story (muted/gray thin arc).
    Read,
    /// Unread story (unread gradient thick arc).
    Unread,
    /// Live video stream (single red/attention ring).
    VideoStream,
}

/// A single arc segment around a peer's userpic ring (`Ui::OutlineSegment` logic).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct RingSegment {
    /// 0-based segment index around the ring (clockwise from 12 o'clock).
    pub index: u32,
    /// Segment visual kind (`Read`, `Unread`, or `VideoStream`).
    pub kind: RingSegmentKind,
}

/// Computes the ordered ring segments for `element` (`List::validateSegments` +
/// `mine && mineUnread` highlight in `dialogs_stories_list.cpp:800-830, 1102-1140`).
///
/// Rules:
/// - If `has_video_stream`: returns a single `VideoStream` segment.
/// - Otherwise returns `count` segments (clamped to `MAX_SEGMENTS_COUNT = 180`):
///   - Indices `0..(count - unread)` are `Read`.
///   - Indices `(count - unread)..count` are `Unread`.
///   - If `mine && mine_unread && count > 0`, the final segment (`count - 1`) is upgraded to
///     `Unread` (matching C++ lines 800-830 where a freshly posted own story highlights the last arc).
pub fn compute_ring_segments(element: &DialogStoryElement) -> Vec<RingSegment> {
    if element.has_video_stream {
        return vec![RingSegment {
            index: 0,
            kind: RingSegmentKind::VideoStream,
        }];
    }
    let count = element.count.min(MAX_SEGMENTS_COUNT);
    if count == 0 {
        return Vec::new();
    }
    let unread = element.unread_count.min(count);
    let read_count = count - unread;
    let mut segments = Vec::with_capacity(count as usize);
    for i in 0..count {
        let kind = if i < read_count {
            RingSegmentKind::Read
        } else {
            RingSegmentKind::Unread
        };
        segments.push(RingSegment { index: i, kind });
    }
    if element.mine
        && element.mine_unread
        && let Some(last) = segments.last_mut()
    {
        last.kind = RingSegmentKind::Unread;
    }
    segments
}

// ---------------------------------------------------------------------------
// Dialog Stories Bar State Machine (`Dialogs::Stories::List`)
// ---------------------------------------------------------------------------

/// Display state of the dialogs-list stories bar (`List::State`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum DialogBarMode {
    /// Collapsed compact overlapping userpics in the search header.
    #[default]
    Small,
    /// Mid-transition between collapsed and expanded while pulling/animating.
    Changing,
    /// Fully expanded horizontal scrollable strip with names and ring segments.
    Full,
}

/// Tooltip content summary for the collapsed stories strip (`List::computeTooltipText`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TooltipSummary {
    /// No stories to show.
    Empty,
    /// Only the current user's own stories are present (`lng_stories_click_to_view_mine`).
    MineOnly,
    /// Up to `MAX_TOOLTIP_NAMES` (3) peer short names (`lng_stories_click_to_view`).
    Peers(Vec<String>),
}

/// Pure-logic state machine for the dialogs-list stories bar (`Dialogs::Stories::List`).
#[derive(Clone, Debug)]
pub struct DialogStoriesBarState {
    content: DialogStoriesContent,
    mode: DialogBarMode,
    expanded: bool,
    last_ratio: f64,
    last_expanded_height: i32,
    full_height: i32,
    viewport_width: i32,
    single_item_width: i32,
    left_padding: i32,
    scroll_left: i32,
    scroll_left_max: i32,
    toggled_hidden: bool,
}

impl DialogStoriesBarState {
    /// Creates a new `DialogStoriesBarState` with layout metrics for scroll & expand calculations.
    pub fn new(full_height: i32, single_item_width: i32, left_padding: i32) -> Self {
        Self {
            content: DialogStoriesContent::default(),
            mode: DialogBarMode::Small,
            expanded: false,
            last_ratio: 0.0,
            last_expanded_height: 0,
            full_height: full_height.max(1),
            viewport_width: 0,
            single_item_width: single_item_width.max(1),
            left_padding: left_padding.max(0),
            scroll_left: 0,
            scroll_left_max: 0,
            toggled_hidden: false,
        }
    }

    /// Replaces the displayed content and recalculates scroll bounds (`List::showContent`).
    pub fn set_content(&mut self, content: DialogStoriesContent) {
        self.content = content;
        self.update_scroll_max();
    }

    /// Returns the current `DialogStoriesContent`.
    pub fn content(&self) -> &DialogStoriesContent {
        &self.content
    }

    /// Returns `true` if there are no elements.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.content.is_empty()
    }

    /// Returns the current display mode (`Small`, `Changing`, or `Full`).
    #[inline]
    pub const fn mode(&self) -> DialogBarMode {
        self.mode
    }

    /// Returns `true` if logically expanded.
    #[inline]
    pub const fn is_expanded(&self) -> bool {
        self.expanded
    }

    /// Returns whether the bar is hidden via toggle.
    #[inline]
    pub const fn is_hidden(&self) -> bool {
        self.toggled_hidden
    }

    /// Sets whether the bar is hidden.
    pub fn set_hidden(&mut self, hidden: bool) {
        self.toggled_hidden = hidden;
    }

    /// Sets the viewport width and updates horizontal scroll bounds (`List::resizeEvent`).
    pub fn set_viewport_width(&mut self, width: i32) {
        self.viewport_width = width.max(0);
        self.update_scroll_max();
    }

    /// Returns current horizontal scroll offset.
    #[inline]
    pub const fn scroll_left(&self) -> i32 {
        self.scroll_left
    }

    /// Returns maximum horizontal scroll offset.
    #[inline]
    pub const fn scroll_left_max(&self) -> i32 {
        self.scroll_left_max
    }

    /// Updates expanded pull height and applies C++ expand/collapse hysteresis
    /// (`List::setExpandedHeight` + `List::updateExpanding` in `dialogs_stories_list.cpp:302-320`).
    ///
    /// Returns `Some(new_expanded)` if the expanded state toggled.
    pub fn set_expanded_height(&mut self, height: i32) -> Option<bool> {
        let clamped = height.clamp(0, self.full_height);
        if self.last_expanded_height == clamped {
            return None;
        }
        self.last_expanded_height = clamped;
        let ratio = if clamped == 0 {
            0.0
        } else {
            f64::from(clamped) / f64::from(self.full_height)
        };
        let expanding = ratio > self.last_ratio;
        self.last_ratio = ratio;

        let mut toggled = None;
        let should_toggle = if self.expanded {
            !expanding && ratio < COLLAPSE_AFTER_RATIO
        } else {
            expanding && ratio > EXPAND_AFTER_RATIO
        };
        if should_toggle {
            self.expanded = !self.expanded;
            toggled = Some(self.expanded);
        }

        self.mode = if clamped == 0 {
            DialogBarMode::Small
        } else if clamped >= self.full_height {
            DialogBarMode::Full
        } else {
            DialogBarMode::Changing
        };
        toggled
    }

    /// Explicitly expands or collapses the strip (e.g. clicking the collapsed strip).
    pub fn set_expanded(&mut self, expanded: bool) {
        self.expanded = expanded;
        self.last_expanded_height = if expanded { self.full_height } else { 0 };
        self.last_ratio = if expanded { 1.0 } else { 0.0 };
        self.mode = if expanded {
            DialogBarMode::Full
        } else {
            DialogBarMode::Small
        };
    }

    /// Scrolls horizontally by `delta_x` pixels (positive scrolls right) and returns whether
    /// `load_more` should be triggered (`List::wheelEvent` / `checkLoadMore`).
    pub fn scroll_by(&mut self, delta_x: i32) -> bool {
        let next = (self.scroll_left + delta_x).clamp(0, self.scroll_left_max);
        self.scroll_left = next;
        self.should_load_more()
    }

    /// Returns `true` when remaining scroll distance is `< viewport_width * PRELOAD_PAGES`
    /// (`List::checkLoadMore` in `dialogs_stories_list.cpp:1267-1271`).
    pub fn should_load_more(&self) -> bool {
        self.scroll_left_max - self.scroll_left < self.viewport_width * PRELOAD_PAGES
    }

    /// Returns the element indices visible in the collapsed (small) header strip
    /// (`computeLayout` in `dialogs_stories_list.cpp:358-373`).
    ///
    /// Skips index 0 when `len > 1 && elements[0].skip_small` (so your own avatar doesn't crowd
    /// out friends' stories in the 3-thumbnail collapsed stack).
    pub fn small_visible_indices(&self) -> Vec<usize> {
        let len = self.content.elements.len();
        if len == 0 {
            return Vec::new();
        }
        let small_skip = if len > 1 && self.content.elements[0].skip_small {
            1
        } else {
            0
        };
        let count = SMALL_THUMBS_SHOWN.min(len - small_skip);
        (small_skip..(small_skip + count)).collect()
    }

    /// Computes the collapsed strip tooltip summary (`List::computeTooltipText`).
    pub fn tooltip_summary(&self) -> TooltipSummary {
        let list = &self.content.elements;
        if list.is_empty() {
            return TooltipSummary::Empty;
        }
        if list.len() == 1 && list[0].skip_small {
            return TooltipSummary::MineOnly;
        }
        let mut names = Vec::with_capacity(MAX_TOOLTIP_NAMES);
        for el in list {
            if el.skip_small {
                continue;
            }
            names.push(el.name.clone());
            if names.len() >= MAX_TOOLTIP_NAMES {
                break;
            }
        }
        if names.is_empty() {
            TooltipSummary::Empty
        } else {
            TooltipSummary::Peers(names)
        }
    }

    /// Computes the interpolated visual expansion ratio matching `List::computeLayout`.
    pub fn visual_expand_ratio(&self, animation_expanded: f64) -> f64 {
        let clamped_anim = animation_expanded.clamp(0.0, 1.0);
        let expanded_ratio = self.last_ratio;
        let collapsed_ratio = expanded_ratio * FRICTION_RATIO;
        let ratio = expanded_ratio * clamped_anim + collapsed_ratio * (1.0 - clamped_anim);
        let lo = EXPAND_AFTER_RATIO * FRICTION_RATIO;
        let hi = COLLAPSE_AFTER_RATIO;
        if ratio >= hi {
            1.0
        } else if ratio <= lo {
            0.0
        } else {
            (ratio - lo) / (hi - lo)
        }
    }

    fn update_scroll_max(&mut self) {
        let full_width =
            self.left_padding + (self.content.elements.len() as i32) * self.single_item_width;
        self.scroll_left_max = (full_width - self.viewport_width).max(0);
        self.scroll_left = self.scroll_left.clamp(0, self.scroll_left_max);
    }
}

/// Convenience helper returning the ring segments for a peer currently stored in `store`.
pub fn peer_ring_segments(
    store: &StoriesStore,
    list: StorySourcesList,
    peer_id: PeerId,
) -> Vec<RingSegment> {
    let content = build_dialog_stories_content(store, list, |_| String::new(), &[]);
    content
        .elements
        .iter()
        .find(|e| e.id == peer_id.raw())
        .map(compute_ring_segments)
        .unwrap_or_default()
}

/// Checks if a given `StoryId` is included in a `DialogStoriesContent` built by `build_last_for_peer`.
pub fn last_for_peer_contains(content: &DialogStoriesContent, story_id: StoryId) -> bool {
    content.elements.iter().any(|e| e.id == story_id as u64)
}
