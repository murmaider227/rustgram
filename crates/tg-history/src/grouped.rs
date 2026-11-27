//! Album / grouped media layout algorithms.
//!
//! Port of:
//! - `Telegram/SourceFiles/ui/grouped_layout.h` & `Telegram/SourceFiles/ui/grouped_layout.cpp`
//!   (`Ui::LayoutMediaGroup`, `Layouter`, `ComplexLayouter`)
//! - `Telegram/SourceFiles/history/view/media/history_view_media_grouped.cpp`
//!   (`LayoutPlaylist`, `GroupedMedia::countOptimalSize`, `GroupedMedia::countCurrentSize`,
//!   `GroupedMedia::applyRoundingSides`, `GroupedMedia::groupedPadding`,
//!   `GroupedMedia::getBubbleSelectionIntervals`)

use crate::consts::{
    FontMetrics, HISTORY_GROUP_SKIP, HISTORY_GROUP_WIDTH_MAX, HISTORY_GROUP_WIDTH_MIN,
    MAX_GROUP_SIZE, MSG_DATE_DELTA, MSG_FILE_LAYOUT, MSG_FILE_LAYOUT_GROUPED, MSG_FILE_TOP_MINUS,
    MSG_PADDING,
};
use crate::geometry::{
    BubbleCornerRounding, BubbleRounding, BubbleSelectionInterval, Margins, Point, Rect, RectParts,
    Size, get_corners_from_sides, mul_div, safe_round,
};
use crate::selection::{TextSelection, is_group_item_selection};

/// Layout result for a single item in a grouped media album (`Ui::GroupMediaLayout`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GroupMediaLayout {
    /// Item rectangle relative to the group origin `(0, 0)`.
    pub geometry: Rect,
    /// Outer edges of the group touched by this item (`RectPart::Top | Left | Right | Bottom`).
    pub sides: RectParts,
}

/// Grouping arrangement mode (`GroupedMedia::Mode` in `history_view_media_grouped.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupedMode {
    /// Photos, videos, and animations arranged in a 2D mosaic grid.
    Grid,
    /// Documents and audio files stacked vertically as a playlist/column.
    Column,
}

/// Hit-test classification for a point relative to a grouped media block (`HistoryView::PointState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupPointState {
    /// Point is outside the grouped media bounding box.
    Outside,
    /// Point is inside the bounding box (e.g. in padding/spacing) but not inside a part.
    Inside,
    /// Point hits a specific part at index `usize`.
    GroupPart(usize),
}

/// Computes the album mosaic layout for a list of media sizes.
///
/// Exact port of `Ui::LayoutMediaGroup` in `Telegram/SourceFiles/ui/grouped_layout.cpp`. Every
/// side is at least 1, as `Photo::sizeForGroupingOptimal` makes it (`history_view_photo.cpp:906`):
/// a zero or negative side from the server would give tiles of negative size.
#[must_use]
pub fn layout_media_group(
    sizes: &[Size],
    max_width: i32,
    min_width: i32,
    spacing: i32,
) -> Vec<GroupMediaLayout> {
    Layouter::new(sizes, max_width, min_width, spacing).layout()
}

/// Stacks column-mode grouped items (documents / audio playlist) vertically at the
/// maximum item width (`LayoutPlaylist` in `history_view_media_grouped.cpp:38-59`).
#[must_use]
pub fn layout_playlist(sizes: &[Size]) -> Vec<GroupMediaLayout> {
    if sizes.is_empty() {
        return Vec::new();
    }
    let width = sizes.iter().map(|s| s.width).max().unwrap_or(0);
    let mut result = Vec::with_capacity(sizes.len());
    let mut top = 0;
    for size in sizes {
        result.push(GroupMediaLayout {
            geometry: Rect::new(0, top, width, size.height),
            sides: RectParts::LEFT | RectParts::RIGHT,
        });
        top = top.saturating_add(size.height);
    }
    if let Some(first) = result.first_mut() {
        first.sides |= RectParts::TOP;
    }
    if let Some(last) = result.last_mut() {
        last.sides |= RectParts::BOTTOM;
    }
    result
}

/// Restricts outer bubble corner rounding to only the corners present in `sides`
/// (`GroupedMedia::applyRoundingSides` in `history_view_media_grouped.cpp:286-303`).
#[must_use]
pub fn apply_rounding_sides(mut already: BubbleRounding, sides: RectParts) -> BubbleRounding {
    let corners = get_corners_from_sides(sides);
    if !corners.contains(RectParts::TOP_LEFT) {
        already.top_left = BubbleCornerRounding::None;
    }
    if !corners.contains(RectParts::TOP_RIGHT) {
        already.top_right = BubbleCornerRounding::None;
    }
    if !corners.contains(RectParts::BOTTOM_LEFT) {
        already.bottom_left = BubbleCornerRounding::None;
    }
    if !corners.contains(RectParts::BOTTOM_RIGHT) {
        already.bottom_right = BubbleCornerRounding::None;
    }
    already
}

/// Computes the vertical padding applied to a grouped media container
/// (`GroupedMedia::groupedPadding` in `history_view_media_grouped.cpp:305-320`).
#[must_use]
pub const fn grouped_padding(
    mode: GroupedMode,
    is_bubble_top: bool,
    is_bubble_bottom: bool,
    last_has_text: bool,
) -> Margins {
    if !matches!(mode, GroupedMode::Column) {
        return Margins::ZERO;
    }
    let normal = MSG_FILE_LAYOUT.padding;
    let grouped = MSG_FILE_LAYOUT_GROUPED.padding;
    let top_minus = if is_bubble_top { 0 } else { MSG_FILE_TOP_MINUS };
    let last_has_caption = is_bubble_bottom && last_has_text;
    let add_to_bottom = if last_has_caption {
        MSG_PADDING.bottom
    } else {
        0
    };
    Margins::new(
        0,
        (normal.top - grouped.top) - top_minus,
        0,
        (normal.bottom - grouped.bottom) + add_to_bottom,
    )
}

/// Input parameters for sizing a `GroupedMedia` block (`history_view_media_grouped.cpp`).
#[derive(Debug, Clone)]
pub struct GroupedMediaInput {
    /// Grid (photos/videos) or Column (files/audio).
    pub mode: GroupedMode,
    /// Natural sizes of each part (truncated to `MAX_GROUP_SIZE = 10` as in tdesktop).
    pub part_sizes: Vec<Size>,
    /// Whether this media is at the top of its bubble.
    pub is_bubble_top: bool,
    /// Whether this media is at the bottom of its bubble.
    pub is_bubble_bottom: bool,
    /// Whether the last part carries caption text.
    pub last_item_has_text: bool,
    /// Whether the bottom info needs an extra row in Column mode
    /// (signed non-rank author, views/replies, or edited badge).
    pub column_extra_bottom_info: bool,
    /// True when hosted inside an Instant View article (disables clamping to `max_width`).
    pub hosted_instant_view: bool,
}

/// Computed layout for a `GroupedMedia` container, supporting both optimal sizing
/// and rescaling to arbitrary widths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupedMediaLayout {
    /// Mode used (`Grid` or `Column`).
    pub mode: GroupedMode,
    /// Optimal bounding size returned by `countOptimalSize()`.
    pub optimal_size: Size,
    /// Current bounding size after `countCurrentSize(new_width)`.
    pub current_size: Size,
    /// Initial (optimal) part layouts from `LayoutMediaGroup` / `LayoutPlaylist`.
    pub initial_parts: Vec<GroupMediaLayout>,
    /// Rescaled part rectangles for the current width (relative to padding top).
    pub parts: Vec<GroupMediaLayout>,
    /// Padding around the parts (`groupedPadding()`).
    pub padding: Margins,
}

impl GroupedMediaLayout {
    /// Computes the optimal grouped media layout (`GroupedMedia::countOptimalSize`).
    #[must_use]
    pub fn compute_optimal(input: &GroupedMediaInput, fonts: &FontMetrics) -> Self {
        let clamped_len = input.part_sizes.len().min(MAX_GROUP_SIZE);
        let sizes = &input.part_sizes[..clamped_len];

        let initial_parts = match input.mode {
            GroupedMode::Grid => layout_media_group(
                sizes,
                HISTORY_GROUP_WIDTH_MAX,
                HISTORY_GROUP_WIDTH_MIN,
                HISTORY_GROUP_SKIP,
            ),
            GroupedMode::Column => layout_playlist(sizes),
        };

        let mut max_width = 0;
        let mut min_height = 0;
        for item in &initial_parts {
            max_width = max_width.max(item.geometry.right());
            min_height = min_height.max(item.geometry.bottom());
        }

        if matches!(input.mode, GroupedMode::Column)
            && input.is_bubble_bottom
            && !input.last_item_has_text
            && input.column_extra_bottom_info
        {
            min_height += fonts.date.height - MSG_DATE_DELTA.y;
        }

        let padding = grouped_padding(
            input.mode,
            input.is_bubble_top,
            input.is_bubble_bottom,
            input.last_item_has_text,
        );
        min_height += padding.vertical();

        let optimal_size = Size::new(max_width, min_height);
        Self {
            mode: input.mode,
            optimal_size,
            current_size: optimal_size,
            parts: initial_parts.clone(),
            initial_parts,
            padding,
        }
    }

    /// Resizes the layout to `new_width` (`GroupedMedia::countCurrentSize`,
    /// `history_view_media_grouped.cpp:209-277`).
    ///
    /// For `GroupedMode::Column`, `column_part_heights` may supply updated per-part heights
    /// at the target width; if omitted, each part retains its optimal height.
    pub fn resize_to_width(
        &mut self,
        mut new_width: i32,
        input: &GroupedMediaInput,
        column_part_heights: Option<&[i32]>,
        fonts: &FontMetrics,
    ) -> Size {
        let hosted_iv = matches!(self.mode, GroupedMode::Grid) && input.hosted_instant_view;
        if !hosted_iv {
            new_width = new_width.min(self.optimal_size.width);
        }

        let mut new_height = 0;
        if matches!(self.mode, GroupedMode::Grid) && new_width < HISTORY_GROUP_WIDTH_MIN {
            self.current_size = Size::new(new_width, 0);
            return self.current_size;
        }

        match self.mode {
            GroupedMode::Column => {
                let mut top = 0;
                for (i, part) in self.parts.iter_mut().enumerate() {
                    let h = column_part_heights
                        .and_then(|hs| hs.get(i).copied())
                        .unwrap_or(self.initial_parts[i].geometry.height);
                    part.geometry = Rect::new(0, top, new_width, h);
                    top += h;
                }
                new_height = top;
            }
            GroupedMode::Grid => {
                let initial_spacing = HISTORY_GROUP_SKIP;
                let max_w = self.optimal_size.width.max(1);
                let factor = f64::from(new_width) / f64::from(max_w);
                let scale = |value: i32| safe_round(f64::from(value) * factor);
                let spacing = scale(initial_spacing);
                for (part, initial) in self.parts.iter_mut().zip(self.initial_parts.iter()) {
                    let sides = initial.sides;
                    let ig = initial.geometry;
                    let need_right_skip = !sides.contains(RectParts::RIGHT);
                    let need_bottom_skip = !sides.contains(RectParts::BOTTOM);
                    let initial_left = ig.x;
                    let initial_top = ig.y;
                    let initial_right =
                        initial_left + ig.width + if need_right_skip { initial_spacing } else { 0 };
                    let initial_bottom = initial_top
                        + ig.height
                        + if need_bottom_skip { initial_spacing } else { 0 };
                    let left = scale(initial_left);
                    let top = scale(initial_top);
                    let width =
                        scale(initial_right) - left - if need_right_skip { spacing } else { 0 };
                    let height =
                        scale(initial_bottom) - top - if need_bottom_skip { spacing } else { 0 };
                    part.geometry = Rect::new(left, top, width, height);
                    new_height = new_height.max(top + height);
                }
            }
        }

        if matches!(self.mode, GroupedMode::Column)
            && input.is_bubble_bottom
            && !input.last_item_has_text
            && input.column_extra_bottom_info
        {
            new_height += fonts.date.height - MSG_DATE_DELTA.y;
        }

        new_height += self.padding.vertical();
        self.current_size = Size::new(new_width, new_height);
        self.current_size
    }

    /// Returns the rectangle of part `index` shifted by `padding.top`
    /// (`GroupedMedia::groupItemRect` in `history_view_media_grouped.cpp:322-329`).
    #[must_use]
    pub fn group_item_rect(&self, index: usize) -> Option<Rect> {
        self.parts
            .get(index)
            .map(|p| p.geometry.translated(0, self.padding.top))
    }

    /// Hit-tests `point` against this grouped media block
    /// (`GroupedMedia::pointState` in `history_view_media_grouped.cpp:534-546`).
    #[must_use]
    pub fn point_state(&self, point: Point) -> GroupPointState {
        let bounds = Rect::new(0, 0, self.current_size.width, self.current_size.height);
        if !bounds.contains(point) {
            return GroupPointState::Outside;
        }
        let shifted = Point::new(point.x, point.y - self.padding.top);
        for (index, part) in self.parts.iter().enumerate() {
            if part.geometry.contains(shifted) {
                return GroupPointState::GroupPart(index);
            }
        }
        GroupPointState::Inside
    }

    /// Computes vertical selection highlight bands for column-mode grouped media
    /// (`GroupedMedia::getBubbleSelectionIntervals` in `history_view_media_grouped.cpp:714-753`).
    #[must_use]
    pub fn bubble_selection_intervals(
        &self,
        selection: TextSelection,
    ) -> Vec<BubbleSelectionInterval> {
        if !matches!(self.mode, GroupedMode::Column) || self.parts.is_empty() {
            return Vec::new();
        }
        let mut result: Vec<BubbleSelectionInterval> = Vec::new();
        for (i, part) in self.parts.iter().enumerate() {
            if !is_group_item_selection(selection, i) {
                continue;
            }
            let g = part.geometry;
            let merge = result.last().is_some_and(|last| {
                !(last.top + last.height < g.top() || last.top > g.top() + g.height)
            });
            if !merge {
                result.push(BubbleSelectionInterval {
                    top: g.top(),
                    height: g.height,
                });
            } else if let Some(last) = result.last_mut() {
                let new_top = last.top.min(g.top());
                let new_height = (last.top + last.height - new_top).max(g.bottom() - new_top);
                *last = BubbleSelectionInterval {
                    top: new_top,
                    height: new_height,
                };
            }
        }
        for band in &mut result {
            band.top += self.padding.top;
        }
        if !result.is_empty() {
            if is_group_item_selection(selection, 0) {
                result[0].top -= self.padding.top;
                result[0].height += self.padding.top;
            }
            if is_group_item_selection(selection, self.parts.len() - 1)
                && let Some(last) = result.last_mut()
            {
                last.height = self.current_size.height - last.top;
            }
        }
        result
    }
}

// ---------------------------------------------------------------------------
// Internal Layouter & ComplexLayouter (exact port of ui/grouped_layout.cpp)
// ---------------------------------------------------------------------------

/// Longest side the album layout computes with. Server sizes are untrusted: a hostile
/// `i32::MAX` side made the i32 sums overflow (TGB-3082). No real photo or video reaches it, so
/// real albums lay out exactly as in tdesktop.
const MAX_MEDIA_SIDE: i64 = 1 << 14;

/// Makes a server media size safe for the layout: every side is at least 1 (as
/// `Photo::sizeForGroupingOptimal` makes it), and a side above [`MAX_MEDIA_SIDE`] scales both
/// sides down keeping the ratio.
fn normalized_media_size(size: Size) -> Size {
    let (width, height) = (i64::from(size.width.max(1)), i64::from(size.height.max(1)));
    let longest = width.max(height);
    if longest <= MAX_MEDIA_SIDE {
        return Size::new(size.width.max(1), size.height.max(1));
    }
    let scale = |side: i64| i32::try_from((side * MAX_MEDIA_SIDE / longest).max(1)).unwrap_or(1);
    Size::new(scale(width), scale(height))
}

struct Layouter {
    sizes: Vec<Size>,
    ratios: Vec<f64>,
    proportions: String,
    count: usize,
    max_width: i32,
    max_height: i32,
    min_width: i32,
    spacing: i32,
    average_ratio: f64,
    max_size_ratio: f64,
}

impl Layouter {
    fn new(sizes: &[Size], max_width: i32, min_width: i32, spacing: i32) -> Self {
        let sizes: Vec<Size> = sizes.iter().copied().map(normalized_media_size).collect();
        let ratios = Self::count_ratios(&sizes);
        let proportions = Self::count_proportions(&ratios);
        let count = ratios.len();
        // All apps currently use square max size first.
        // In complex case they use maxWidth * 4 / 3 as maxHeight.
        let max_height = max_width;
        // Exact port of `ranges::accumulate(_ratios, 1.) / _count` (grouped_layout.cpp:103):
        // note that tdesktop seeds the fold with `1.0` rather than `0.0`.
        let average_ratio = if count == 0 {
            1.0
        } else {
            ratios.iter().fold(1.0, |acc, &r| acc + r) / (count as f64)
        };
        let max_size_ratio = if max_height != 0 {
            f64::from(max_width) / f64::from(max_height)
        } else {
            1.0
        };
        Self {
            sizes,
            ratios,
            proportions,
            count,
            max_width,
            max_height,
            min_width,
            spacing,
            average_ratio,
            max_size_ratio,
        }
    }

    fn count_ratios(sizes: &[Size]) -> Vec<f64> {
        sizes
            .iter()
            .map(|size| f64::from(size.width) / f64::from(size.height))
            .collect()
    }

    fn count_proportions(ratios: &[f64]) -> String {
        ratios
            .iter()
            .map(|&ratio| {
                if ratio > 1.2 {
                    'w'
                } else if ratio < 0.8 {
                    'n'
                } else {
                    'q'
                }
            })
            .collect()
    }

    fn layout(&self) -> Vec<GroupMediaLayout> {
        if self.count == 0 {
            return Vec::new();
        }
        if self.count == 1 {
            return self.layout_one();
        }
        if self.count >= 5 || self.ratios.iter().any(|&r| r > 2.0) {
            return ComplexLayouter::new(
                &self.ratios,
                self.average_ratio,
                self.max_width,
                self.min_width,
                self.spacing,
            )
            .layout();
        }
        match self.count {
            2 => self.layout_two(),
            3 => self.layout_three(),
            _ => self.layout_four(),
        }
    }

    fn layout_one(&self) -> Vec<GroupMediaLayout> {
        let width = self.max_width;
        let first = self.sizes[0];
        let height = mul_div(first.height, width, first.width);
        vec![GroupMediaLayout {
            geometry: Rect::new(0, 0, width, height),
            sides: RectParts::ALL_SIDES,
        }]
    }

    fn layout_two(&self) -> Vec<GroupMediaLayout> {
        if self.proportions == "ww"
            && self.average_ratio > 1.4 * self.max_size_ratio
            && (self.ratios[1] - self.ratios[0]) < 0.2
        {
            self.layout_two_top_bottom()
        } else if self.proportions == "ww" || self.proportions == "qq" {
            self.layout_two_left_right_equal()
        } else {
            self.layout_two_left_right()
        }
    }

    fn layout_three(&self) -> Vec<GroupMediaLayout> {
        if self.proportions.starts_with('n') {
            self.layout_three_left_and_other()
        } else {
            self.layout_three_top_and_other()
        }
    }

    fn layout_four(&self) -> Vec<GroupMediaLayout> {
        if self.proportions.starts_with('w') {
            self.layout_four_top_and_other()
        } else {
            self.layout_four_left_and_other()
        }
    }

    fn layout_two_top_bottom(&self) -> Vec<GroupMediaLayout> {
        let width = self.max_width;
        let height = safe_round(
            (f64::from(width) / self.ratios[0]).min(
                (f64::from(width) / self.ratios[1])
                    .min(f64::from(self.max_height - self.spacing) / 2.0),
            ),
        );
        vec![
            GroupMediaLayout {
                geometry: Rect::new(0, 0, width, height),
                sides: RectParts::LEFT | RectParts::TOP | RectParts::RIGHT,
            },
            GroupMediaLayout {
                geometry: Rect::new(0, height + self.spacing, width, height),
                sides: RectParts::LEFT | RectParts::BOTTOM | RectParts::RIGHT,
            },
        ]
    }

    fn layout_two_left_right_equal(&self) -> Vec<GroupMediaLayout> {
        let width = (self.max_width - self.spacing) / 2;
        let height = safe_round(
            (f64::from(width) / self.ratios[0])
                .min((f64::from(width) / self.ratios[1]).min(f64::from(self.max_height))),
        );
        vec![
            GroupMediaLayout {
                geometry: Rect::new(0, 0, width, height),
                sides: RectParts::TOP | RectParts::LEFT | RectParts::BOTTOM,
            },
            GroupMediaLayout {
                geometry: Rect::new(width + self.spacing, 0, width, height),
                sides: RectParts::TOP | RectParts::RIGHT | RectParts::BOTTOM,
            },
        ]
    }

    fn layout_two_left_right(&self) -> Vec<GroupMediaLayout> {
        let minimal_width = safe_round(f64::from(self.min_width) * 1.5);
        let available = f64::from(self.max_width - self.spacing);
        let second_width = safe_round(
            (0.4 * available)
                .max((available / self.ratios[0]) / (1.0 / self.ratios[0] + 1.0 / self.ratios[1])),
        )
        .min(self.max_width - self.spacing - minimal_width);
        let first_width = self.max_width - second_width - self.spacing;
        let height = self.max_height.min(safe_round(
            (f64::from(first_width) / self.ratios[0]).min(f64::from(second_width) / self.ratios[1]),
        ));
        vec![
            GroupMediaLayout {
                geometry: Rect::new(0, 0, first_width, height),
                sides: RectParts::TOP | RectParts::LEFT | RectParts::BOTTOM,
            },
            GroupMediaLayout {
                geometry: Rect::new(first_width + self.spacing, 0, second_width, height),
                sides: RectParts::TOP | RectParts::RIGHT | RectParts::BOTTOM,
            },
        ]
    }

    fn layout_three_left_and_other(&self) -> Vec<GroupMediaLayout> {
        let first_height = self.max_height;
        let third_height = safe_round((f64::from(self.max_height - self.spacing) / 2.0).min(
            self.ratios[1] * f64::from(self.max_width - self.spacing)
                / (self.ratios[2] + self.ratios[1]),
        ));
        let second_height = first_height - third_height - self.spacing;
        let right_width = self.min_width.max(safe_round(
            (f64::from(self.max_width - self.spacing) / 2.0).min(
                (f64::from(third_height) * self.ratios[2])
                    .min(f64::from(second_height) * self.ratios[1]),
            ),
        ));
        let left_width = safe_round(f64::from(first_height) * self.ratios[0])
            .min(self.max_width - self.spacing - right_width);
        vec![
            GroupMediaLayout {
                geometry: Rect::new(0, 0, left_width, first_height),
                sides: RectParts::TOP | RectParts::LEFT | RectParts::BOTTOM,
            },
            GroupMediaLayout {
                geometry: Rect::new(left_width + self.spacing, 0, right_width, second_height),
                sides: RectParts::TOP | RectParts::RIGHT,
            },
            GroupMediaLayout {
                geometry: Rect::new(
                    left_width + self.spacing,
                    second_height + self.spacing,
                    right_width,
                    third_height,
                ),
                sides: RectParts::BOTTOM | RectParts::RIGHT,
            },
        ]
    }

    fn layout_three_top_and_other(&self) -> Vec<GroupMediaLayout> {
        let first_width = self.max_width;
        let first_height = safe_round(
            (f64::from(first_width) / self.ratios[0])
                .min(f64::from(self.max_height - self.spacing) * 0.66),
        );
        let second_width = (self.max_width - self.spacing) / 2;
        let second_height = (self.max_height - first_height - self.spacing).min(safe_round(
            (f64::from(second_width) / self.ratios[1])
                .min(f64::from(second_width) / self.ratios[2]),
        ));
        let third_width = first_width - second_width - self.spacing;
        vec![
            GroupMediaLayout {
                geometry: Rect::new(0, 0, first_width, first_height),
                sides: RectParts::LEFT | RectParts::TOP | RectParts::RIGHT,
            },
            GroupMediaLayout {
                geometry: Rect::new(0, first_height + self.spacing, second_width, second_height),
                sides: RectParts::BOTTOM | RectParts::LEFT,
            },
            GroupMediaLayout {
                geometry: Rect::new(
                    second_width + self.spacing,
                    first_height + self.spacing,
                    third_width,
                    second_height,
                ),
                sides: RectParts::BOTTOM | RectParts::RIGHT,
            },
        ]
    }

    fn layout_four_top_and_other(&self) -> Vec<GroupMediaLayout> {
        let w = self.max_width;
        let h0 = safe_round(
            (f64::from(w) / self.ratios[0]).min(f64::from(self.max_height - self.spacing) * 0.66),
        );
        let h = safe_round(
            f64::from(self.max_width - 2 * self.spacing)
                / (self.ratios[1] + self.ratios[2] + self.ratios[3]),
        );
        let w0 = self.min_width.max(safe_round(
            (f64::from(self.max_width - 2 * self.spacing) * 0.4).min(f64::from(h) * self.ratios[1]),
        ));
        let w2 = safe_round(
            f64::from(self.min_width)
                .max(f64::from(self.max_width - 2 * self.spacing) * 0.33)
                .max(f64::from(h) * self.ratios[3]),
        );
        let w1 = w - w0 - w2 - 2 * self.spacing;
        let h1 = (self.max_height - h0 - self.spacing).min(h);
        vec![
            GroupMediaLayout {
                geometry: Rect::new(0, 0, w, h0),
                sides: RectParts::LEFT | RectParts::TOP | RectParts::RIGHT,
            },
            GroupMediaLayout {
                geometry: Rect::new(0, h0 + self.spacing, w0, h1),
                sides: RectParts::BOTTOM | RectParts::LEFT,
            },
            GroupMediaLayout {
                geometry: Rect::new(w0 + self.spacing, h0 + self.spacing, w1, h1),
                sides: RectParts::BOTTOM,
            },
            GroupMediaLayout {
                geometry: Rect::new(
                    w0 + self.spacing + w1 + self.spacing,
                    h0 + self.spacing,
                    w2,
                    h1,
                ),
                sides: RectParts::RIGHT | RectParts::BOTTOM,
            },
        ]
    }

    fn layout_four_left_and_other(&self) -> Vec<GroupMediaLayout> {
        let h = self.max_height;
        let w0 = safe_round(
            (f64::from(h) * self.ratios[0]).min(f64::from(self.max_width - self.spacing) * 0.6),
        );
        let w = safe_round(
            f64::from(self.max_height - 2 * self.spacing)
                / (1.0 / self.ratios[1] + 1.0 / self.ratios[2] + 1.0 / self.ratios[3]),
        );
        let h0 = safe_round(f64::from(w) / self.ratios[1]);
        let h1 = safe_round(f64::from(w) / self.ratios[2]);
        let h2 = h - h0 - h1 - 2 * self.spacing;
        let w1 = self
            .min_width
            .max((self.max_width - w0 - self.spacing).min(w));
        vec![
            GroupMediaLayout {
                geometry: Rect::new(0, 0, w0, h),
                sides: RectParts::TOP | RectParts::LEFT | RectParts::BOTTOM,
            },
            GroupMediaLayout {
                geometry: Rect::new(w0 + self.spacing, 0, w1, h0),
                sides: RectParts::TOP | RectParts::RIGHT,
            },
            GroupMediaLayout {
                geometry: Rect::new(w0 + self.spacing, h0 + self.spacing, w1, h1),
                sides: RectParts::RIGHT,
            },
            GroupMediaLayout {
                geometry: Rect::new(w0 + self.spacing, h0 + h1 + 2 * self.spacing, w1, h2),
                sides: RectParts::BOTTOM | RectParts::RIGHT,
            },
        ]
    }
}

struct ComplexAttempt {
    line_counts: Vec<usize>,
    heights: Vec<f64>,
}

struct ComplexLayouter {
    ratios: Vec<f64>,
    count: usize,
    max_width: i32,
    max_height: i32,
    min_width: i32,
    spacing: i32,
    average_ratio: f64,
}

impl ComplexLayouter {
    fn new(
        ratios: &[f64],
        average_ratio: f64,
        max_width: i32,
        min_width: i32,
        spacing: i32,
    ) -> Self {
        let cropped = Self::crop_ratios(ratios, average_ratio);
        let count = cropped.len();
        Self {
            ratios: cropped,
            count,
            max_width,
            max_height: (max_width * 4) / 3,
            min_width,
            spacing,
            average_ratio,
        }
    }

    fn crop_ratios(ratios: &[f64], average_ratio: f64) -> Vec<f64> {
        const MAX_RATIO: f64 = 2.75;
        const MIN_RATIO: f64 = 0.6667;
        ratios
            .iter()
            .map(|&ratio| {
                if average_ratio > 1.1 {
                    ratio.clamp(1.0, MAX_RATIO)
                } else {
                    ratio.clamp(MIN_RATIO, 1.0)
                }
            })
            .collect()
    }

    fn multi_height(&self, offset: usize, count: usize) -> f64 {
        let sum: f64 = self.ratios[offset..offset + count].iter().sum();
        f64::from(self.max_width - (count as i32 - 1) * self.spacing) / sum
    }

    fn push_attempt(&self, attempts: &mut Vec<ComplexAttempt>, line_counts: Vec<usize>) {
        let mut heights = Vec::with_capacity(line_counts.len());
        let mut offset = 0;
        for &count in &line_counts {
            heights.push(self.multi_height(offset, count));
            offset += count;
        }
        attempts.push(ComplexAttempt {
            line_counts,
            heights,
        });
    }

    fn layout(&self) -> Vec<GroupMediaLayout> {
        let mut result = vec![GroupMediaLayout::default(); self.count];
        let mut attempts = Vec::new();

        // 2-row partitions.
        for first in 1..self.count {
            let second = self.count - first;
            if first > 3 || second > 3 {
                continue;
            }
            self.push_attempt(&mut attempts, vec![first, second]);
        }

        // 3-row partitions.
        let max_second_in_three = if self.average_ratio < 0.85 { 4 } else { 3 };
        if self.count >= 3 {
            for first in 1..(self.count - 1) {
                for second in 1..(self.count - first) {
                    let third = self.count - first - second;
                    if first > 3 || second > max_second_in_three || third > 3 {
                        continue;
                    }
                    self.push_attempt(&mut attempts, vec![first, second, third]);
                }
            }
        }

        // 4-row partitions.
        if self.count >= 4 {
            for first in 1..(self.count - 1) {
                for second in 1..(self.count - first) {
                    for third in 1..(self.count - first - second) {
                        let fourth = self.count - first - second - third;
                        if first > 3 || second > 3 || third > 3 || fourth > 3 {
                            continue;
                        }
                        self.push_attempt(&mut attempts, vec![first, second, third, fourth]);
                    }
                }
            }
        }

        // Defensive fallback if caller passes more than 12 items (Telegram caps at 10).
        if attempts.is_empty() {
            let mut rem = self.count;
            let mut counts = Vec::new();
            while rem > 0 {
                let take = rem.min(3);
                counts.push(take);
                rem -= take;
            }
            self.push_attempt(&mut attempts, counts);
        }

        let mut optimal_idx = 0usize;
        let mut optimal_diff = f64::INFINITY;
        for (idx, attempt) in attempts.iter().enumerate() {
            let heights = &attempt.heights;
            let counts = &attempt.line_counts;
            let line_count = counts.len();
            let total_height: f64 =
                heights.iter().sum::<f64>() + f64::from(self.spacing * (line_count as i32 - 1));
            let min_line_height = heights.iter().copied().fold(f64::INFINITY, f64::min);
            let bad1 = if min_line_height < f64::from(self.min_width) {
                1.5
            } else {
                1.0
            };
            let bad2 = if (1..line_count).any(|line| counts[line - 1] > counts[line]) {
                1.5
            } else {
                1.0
            };
            let diff = (total_height - f64::from(self.max_height)).abs() * bad1 * bad2;
            if idx == 0 || diff < optimal_diff {
                optimal_idx = idx;
                optimal_diff = diff;
            }
        }

        let optimal = &attempts[optimal_idx];
        let row_count = optimal.line_counts.len();
        let mut index = 0usize;
        let mut y = 0i32;

        for row in 0..row_count {
            let col_count = optimal.line_counts[row];
            let line_height = optimal.heights[row];
            let height = safe_round(line_height);
            let mut x = 0i32;
            for col in 0..col_count {
                let mut sides = RectParts::NONE;
                if row == 0 {
                    sides |= RectParts::TOP;
                }
                if row + 1 == row_count {
                    sides |= RectParts::BOTTOM;
                }
                if col == 0 {
                    sides |= RectParts::LEFT;
                }
                if col + 1 == col_count {
                    sides |= RectParts::RIGHT;
                }

                let ratio = self.ratios[index];
                let width = if col + 1 == col_count {
                    self.max_width - x
                } else {
                    safe_round(ratio * line_height)
                };
                result[index] = GroupMediaLayout {
                    geometry: Rect::new(x, y, width, height),
                    sides,
                };
                x += width + self.spacing;
                index += 1;
            }
            y += height + self.spacing;
        }

        result
    }
}
