//! Album / gallery group thumbnail strip for the fullscreen media viewer overlay.
//!
//! Port of:
//! - `Telegram/SourceFiles/media/view/media_view_group_thumbs.h`
//! - `Telegram/SourceFiles/media/view/media_view_group_thumbs.cpp`

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2,
};
use tg_ui::fonts::font_id_from_style;
use tg_ui::tg_style::st;

use crate::media::backend::{MediaItem, MediaItemKind};

/// Normal thumbnail tile width in pixels (`st::mvGroupThumbSize`).
pub const GROUP_THUMB_WIDTH: f32 = 36.0;

/// Expanded active thumbnail tile width in pixels (tdesktop widens active tile for emphasis).
pub const GROUP_THUMB_ACTIVE_WIDTH: f32 = 52.0;

/// Thumbnail tile height in pixels.
pub const GROUP_THUMB_HEIGHT: f32 = 44.0;

/// Horizontal spacing between adjacent thumbnails.
pub const GROUP_THUMB_GAP: f32 = 4.0;

/// Maximum visible thumbnails painted symmetrically around the active item.
pub const GROUP_THUMBS_MAX_VISIBLE: usize = 11;

/// Gallery indices whose thumbnails the strip shows for the item at `index`
/// (`GroupThumbs::RefreshFromSlice` with `ComputeContext` = the message's `groupId`): the run of
/// neighbours that share the current item's album id. `None` when the item is not in an album or
/// its album has a single item, so the strip stays hidden.
#[must_use]
pub fn album_strip_range(items: &[MediaItem], index: usize) -> Option<std::ops::Range<usize>> {
    let group = items.get(index)?.grouped_id?;
    let same = |i: usize| items.get(i).and_then(|it| it.grouped_id) == Some(group);
    let mut from = index;
    while from > 0 && same(from - 1) {
        from -= 1;
    }
    let mut till = index + 1;
    while same(till) {
        till += 1;
    }
    (from + 1 < till).then_some(from..till)
}

/// Computed layout rectangle for a single thumbnail in [`GroupThumbsStrip`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThumbSlot {
    /// Gallery index of the sibling item.
    pub index: usize,
    /// Painted rectangle in viewport coordinates.
    pub rect: Rect,
    /// Whether this slot is the active item.
    pub active: bool,
}

/// Stateless/lightweight strip calculator and renderer for media viewer album thumbnails.
#[derive(Debug, Default)]
pub struct GroupThumbsStrip {
    last_slots: Vec<ThumbSlot>,
}

impl GroupThumbsStrip {
    /// Creates an empty [`GroupThumbsStrip`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            last_slots: Vec::new(),
        }
    }

    /// Thumbnail slots painted by the last [`Self::show`] (empty while the strip is hidden).
    #[must_use]
    pub fn slots(&self) -> &[ThumbSlot] {
        &self.last_slots
    }

    /// Forgets the painted slots when the strip is hidden.
    pub fn clear(&mut self) {
        self.last_slots.clear();
    }

    /// Returns the total strip height including vertical padding.
    #[must_use]
    pub const fn height() -> f32 {
        GROUP_THUMB_HEIGHT + 12.0
    }

    /// Computes centered horizontal [`ThumbSlot`] rects around `active_index` within `strip_rect`.
    #[must_use]
    pub fn layout_slots(
        total_items: usize,
        active_index: usize,
        strip_rect: Rect,
    ) -> Vec<ThumbSlot> {
        if total_items <= 1 || strip_rect.width() < GROUP_THUMB_ACTIVE_WIDTH {
            return Vec::new();
        }

        let max_by_width = (((strip_rect.width() - GROUP_THUMB_ACTIVE_WIDTH)
            / (GROUP_THUMB_WIDTH + GROUP_THUMB_GAP))
            .floor() as usize)
            + 1;
        let visible_count = total_items
            .min(GROUP_THUMBS_MAX_VISIBLE)
            .min(max_by_width.max(1));

        let active = active_index.min(total_items - 1);
        let half = visible_count / 2;
        let mut start = active.saturating_sub(half);
        if start + visible_count > total_items {
            start = total_items - visible_count;
        }
        let end = start + visible_count;

        let total_w: f32 = (start..end)
            .map(|i| {
                if i == active {
                    GROUP_THUMB_ACTIVE_WIDTH
                } else {
                    GROUP_THUMB_WIDTH
                }
            })
            .sum::<f32>()
            + (visible_count.saturating_sub(1) as f32) * GROUP_THUMB_GAP;

        let mut x = strip_rect.center().x - total_w * 0.5;
        let y = strip_rect.center().y - GROUP_THUMB_HEIGHT * 0.5;
        let mut slots = Vec::with_capacity(visible_count);

        for idx in start..end {
            let is_active = idx == active;
            let w = if is_active {
                GROUP_THUMB_ACTIVE_WIDTH
            } else {
                GROUP_THUMB_WIDTH
            };
            slots.push(ThumbSlot {
                index: idx,
                rect: Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, GROUP_THUMB_HEIGHT)),
                active: is_active,
            });
            x += w + GROUP_THUMB_GAP;
        }

        slots
    }

    /// Renders the thumbnail strip inside `strip_rect` and returns the clicked gallery index if any.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        strip_rect: Rect,
        items: &[MediaItem],
        active_index: usize,
    ) -> Option<usize> {
        self.last_slots = Self::layout_slots(items.len(), active_index, strip_rect);
        if self.last_slots.is_empty() {
            return None;
        }

        let mut clicked = None;
        let painter = ui.painter();

        for slot in &self.last_slots {
            let item = &items[slot.index];
            let resp = ui.interact(
                slot.rect,
                ui.scope_id()
                    .with(("mv_group_thumb", item.id.0, slot.index)),
                Sense::click(),
            );
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            if resp.clicked() {
                clicked = Some(slot.index);
            }

            let base_fill = thumb_placeholder_color(item);
            let alpha = if slot.active || resp.hovered() {
                255
            } else {
                185
            };
            let fill =
                Color32::from_rgba_unmultiplied(base_fill.r(), base_fill.g(), base_fill.b(), alpha);
            painter.rect_filled(slot.rect, CornerRadius::same(4), fill);

            // Kind badge / index indicator inside each thumbnail tile.
            let fg =
                Color32::from_rgba_unmultiplied(255, 255, 255, if slot.active { 245 } else { 190 });
            match item.kind {
                MediaItemKind::Video => {
                    tg_app_core::ui_icons::paint_triangle(
                        painter,
                        slot.rect.center() + Vec2::new(1.0, 0.0),
                        12.0,
                        tg_app_core::ui_icons::ArrowDir::Right,
                        fg,
                    );
                }
                MediaItemKind::RoundVideo => {
                    painter.circle_filled(slot.rect.center(), 5.0, fg);
                }
                _ => {
                    let label = match item.kind {
                        MediaItemKind::Gif => "GIF".to_owned(),
                        MediaItemKind::VideoStream => "LIVE".to_owned(),
                        MediaItemKind::Theme => "THM".to_owned(),
                        MediaItemKind::Document => "DOC".to_owned(),
                        MediaItemKind::Photo => format!("{}", slot.index + 1),
                        MediaItemKind::Video | MediaItemKind::RoundVideo => unreachable!(),
                    };
                    painter.text(
                        slot.rect.center(),
                        Align2::CENTER_CENTER,
                        label,
                        font_id_from_style(st::semibold_font),
                        fg,
                    );
                }
            }

            if slot.active {
                painter.rect_stroke(
                    slot.rect,
                    CornerRadius::same(4),
                    Stroke::new(2.0, Color32::WHITE),
                    StrokeKind::Inside,
                );
            } else if resp.hovered() {
                painter.rect_stroke(
                    slot.rect,
                    CornerRadius::same(4),
                    Stroke::new(1.0, Color32::from_white_alpha(180)),
                    StrokeKind::Inside,
                );
            }
        }

        clicked
    }

    /// Returns the most recently laid-out [`ThumbSlot`]s (for hit-testing & unit tests).
    #[must_use]
    pub fn last_slots(&self) -> &[ThumbSlot] {
        &self.last_slots
    }
}

fn thumb_placeholder_color(item: &MediaItem) -> Color32 {
    const SWATCHES: [Color32; 6] = [
        Color32::from_rgb(0x3B, 0x6B, 0x99),
        Color32::from_rgb(0x4E, 0x7A, 0x5B),
        Color32::from_rgb(0x7D, 0x56, 0x8F),
        Color32::from_rgb(0x9A, 0x67, 0x3D),
        Color32::from_rgb(0x3F, 0x7E, 0x82),
        Color32::from_rgb(0x8A, 0x4B, 0x5E),
    ];
    SWATCHES[(item.id.0 as usize) % SWATCHES.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_slots_centers_active_and_expands_width() {
        let strip = Rect::from_min_size(Pos2::new(0.0, 500.0), Vec2::new(600.0, 56.0));
        assert!(GroupThumbsStrip::layout_slots(1, 0, strip).is_empty());

        let slots = GroupThumbsStrip::layout_slots(5, 2, strip);
        assert_eq!(slots.len(), 5);
        assert!(slots[2].active);
        assert_eq!(slots[2].rect.width(), GROUP_THUMB_ACTIVE_WIDTH);
        assert_eq!(slots[0].rect.width(), GROUP_THUMB_WIDTH);

        // Centered around strip center X (300.0).
        let center_x =
            (slots.first().unwrap().rect.left() + slots.last().unwrap().rect.right()) * 0.5;
        assert!((center_x - 300.0).abs() < 0.01);
    }

    #[test]
    fn layout_slots_respects_max_visible_window() {
        let strip = Rect::from_min_size(Pos2::new(0.0, 500.0), Vec2::new(900.0, 56.0));
        let slots = GroupThumbsStrip::layout_slots(25, 12, strip);
        assert_eq!(slots.len(), GROUP_THUMBS_MAX_VISIBLE);
        assert_eq!(slots.first().unwrap().index, 7);
        assert_eq!(slots.last().unwrap().index, 17);
        assert!(slots.iter().any(|s| s.index == 12 && s.active));
    }
}
