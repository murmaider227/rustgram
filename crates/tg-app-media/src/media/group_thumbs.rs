//! Album thumbnail strip at the bottom of the fullscreen media viewer.
//!
//! Port of:
//! - `Telegram/SourceFiles/media/view/media_view_group_thumbs.h`
//! - `Telegram/SourceFiles/media/view/media_view_group_thumbs.cpp`

use std::collections::HashMap;

use egui::{Color32, CursorIcon, Pos2, Rect, Sense, TextureHandle, TextureOptions, Ui, Vec2};
use image::DynamicImage;
use image::imageops::FilterType;
use tg_ui::tg_style::st;

use crate::media::backend::{MediaItem, MediaItemId};
use crate::media::style::MEDIAVIEW_MENU_BG;

/// Width of a thumbnail that is not the current item (`mediaviewGroupWidth`).
pub const GROUP_THUMB_WIDTH: f32 = st::mediaview_group_width.0 as f32;

/// Widest the current item's thumbnail may get (`mediaviewGroupWidthMax`).
pub const GROUP_THUMB_WIDTH_MAX: f32 = st::mediaview_group_width_max.0 as f32;

/// Thumbnail height (`mediaviewGroupHeight`).
pub const GROUP_THUMB_HEIGHT: f32 = st::mediaview_group_height.0 as f32;

/// Gap on each side of a thumbnail (`mediaviewGroupSkip`).
pub const GROUP_THUMB_SKIP: f32 = st::mediaview_group_skip.0 as f32;

/// Gap on each side of the current thumbnail (`mediaviewGroupSkipCurrent`).
pub const GROUP_THUMB_SKIP_CURRENT: f32 = st::mediaview_group_skip_current.0 as f32;

/// Space above the thumbnails (`mediaviewGroupPadding.top`).
const GROUP_PADDING_TOP: f32 = st::mediaview_group_padding.top.0 as f32;

/// Space below the thumbnails (`mediaviewGroupPadding.bottom`).
const GROUP_PADDING_BOTTOM: f32 = st::mediaview_group_padding.bottom.0 as f32;

/// Opacity of the thumbnails around the current one (`Thumb::setState(State::Alive)`).
const ALIVE_OPACITY: f32 = 0.7;

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

/// How many thumbnails fit on each side of the current one (`ComputeThumbsLimit`).
#[must_use]
pub fn thumbs_limit(available_width: f32) -> usize {
    let single = GROUP_THUMB_WIDTH + 2.0 * GROUP_THUMB_SKIP;
    let current = GROUP_THUMB_WIDTH_MAX + 2.0 * GROUP_THUMB_SKIP_CURRENT;
    let left = available_width - current - 2.0 * single;
    ((left / (2.0 * single)).floor() as i64).max(1) as usize
}

/// Width of the current item's thumbnail: its aspect ratio at `mediaviewGroupHeight`, at most
/// `mediaviewGroupWidthMax` (`Thumb::validateImage`).
#[must_use]
pub fn current_thumb_width(item: &MediaItem) -> f32 {
    let w = item.width.max(1) as f32;
    let h = item.height.max(1) as f32;
    (w * GROUP_THUMB_HEIGHT / h)
        .round()
        .clamp(1.0, GROUP_THUMB_WIDTH_MAX)
}

/// Computed layout rectangle for a single thumbnail in [`GroupThumbsStrip`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThumbSlot {
    /// Index of the item in the slice given to [`GroupThumbsStrip::show`].
    pub index: usize,
    /// Painted rectangle in viewport coordinates.
    pub rect: Rect,
    /// Whether this slot is the current item.
    pub active: bool,
}

/// A decoded thumbnail: `mediaviewGroupHeight` tall, centre-cropped to `mediaviewGroupWidthMax`.
struct ThumbTexture {
    texture: TextureHandle,
    /// Logical width of the full thumbnail (`Thumb::_fullWidth`).
    full_width: f32,
}

/// Album thumbnail strip (`Media::View::GroupThumbs`).
#[derive(Default)]
pub struct GroupThumbsStrip {
    last_slots: Vec<ThumbSlot>,
    thumbs: HashMap<MediaItemId, ThumbTexture>,
}

impl std::fmt::Debug for GroupThumbsStrip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GroupThumbsStrip")
            .field("last_slots", &self.last_slots)
            .field("thumbs", &self.thumbs.len())
            .finish()
    }
}

impl GroupThumbsStrip {
    /// Creates an empty [`GroupThumbsStrip`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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

    /// Whether a thumbnail picture is cached for `id`.
    #[must_use]
    pub fn has_thumb(&self, id: MediaItemId) -> bool {
        self.thumbs.contains_key(&id)
    }

    /// Strip height including its vertical padding (`GroupThumbs::height`).
    #[must_use]
    pub const fn height() -> f32 {
        GROUP_PADDING_TOP + GROUP_THUMB_HEIGHT + GROUP_PADDING_BOTTOM
    }

    /// Keeps a thumbnail of a decoded album photo so the strip can show it
    /// (`Thumb::validateImage`: scaled to `mediaviewGroupHeight`, wider pictures centre-cropped
    /// to `mediaviewGroupWidthMax`).
    pub fn remember(&mut self, ctx: &egui::Context, id: MediaItemId, image: &DynamicImage) {
        if self.thumbs.contains_key(&id) {
            return;
        }
        let ppp = ctx.pixels_per_point().max(1.0);
        let (w0, h0) = (image.width().max(1), image.height().max(1));
        let full_width = (w0 as f32 * GROUP_THUMB_HEIGHT / h0 as f32)
            .round()
            .max(1.0);
        let px_h = (GROUP_THUMB_HEIGHT * ppp).round().max(1.0) as u32;
        let scaled = if full_width > GROUP_THUMB_WIDTH_MAX {
            let take = ((w0 as f32 * GROUP_THUMB_WIDTH_MAX / full_width) as u32).clamp(1, w0);
            let px_w = (GROUP_THUMB_WIDTH_MAX * ppp).round().max(1.0) as u32;
            image.crop_imm((w0 - take) / 2, 0, take, h0).resize_exact(
                px_w,
                px_h,
                FilterType::Triangle,
            )
        } else {
            let px_w = (full_width * ppp).round().max(1.0) as u32;
            image.resize_exact(px_w, px_h, FilterType::Triangle)
        };
        let rgba = scaled.to_rgba8();
        let color = egui::ColorImage::from_rgba_unmultiplied(
            [rgba.width() as usize, rgba.height() as usize],
            rgba.as_raw(),
        );
        let texture = ctx.load_texture(
            format!("tg_media_view_group_thumb_{}", id.0),
            color,
            TextureOptions::LINEAR,
        );
        self.thumbs.insert(
            id,
            ThumbTexture {
                texture,
                full_width: full_width.min(GROUP_THUMB_WIDTH_MAX),
            },
        );
    }

    /// Thumbnail rectangles inside `strip_rect` (`GroupThumbs::animateAliveItems` final state):
    /// the current item centred at its own width, `mediaviewGroupWidth` items beside it,
    /// `mediaviewGroupSkip` around each and `mediaviewGroupSkipCurrent` around the current one,
    /// at most [`thumbs_limit`] items on each side.
    #[must_use]
    pub fn layout_slots(
        items: &[MediaItem],
        active_index: usize,
        strip_rect: Rect,
    ) -> Vec<ThumbSlot> {
        if items.len() <= 1 || active_index >= items.len() {
            return Vec::new();
        }
        let limit = thumbs_limit(strip_rect.width());
        let from = active_index.saturating_sub(limit);
        let till = (active_index + limit + 1).min(items.len());
        let y = strip_rect.min.y + GROUP_PADDING_TOP;
        let size = |w: f32| Vec2::new(w, GROUP_THUMB_HEIGHT);

        let current_w = current_thumb_width(&items[active_index]);
        let current_left = strip_rect.center().x.round() - (current_w / 2.0).floor();
        let mut slots = Vec::with_capacity(till - from);
        slots.push(ThumbSlot {
            index: active_index,
            rect: Rect::from_min_size(Pos2::new(current_left, y), size(current_w)),
            active: true,
        });

        let mut next_left = current_left;
        for index in (from..active_index).rev() {
            let skip = if index + 1 == active_index {
                GROUP_THUMB_SKIP_CURRENT
            } else {
                GROUP_THUMB_SKIP
            };
            let left = next_left - GROUP_THUMB_WIDTH - GROUP_THUMB_SKIP - skip;
            slots.push(ThumbSlot {
                index,
                rect: Rect::from_min_size(Pos2::new(left, y), size(GROUP_THUMB_WIDTH)),
                active: false,
            });
            next_left = left;
        }

        let mut prev_right = current_left + current_w;
        for index in active_index + 1..till {
            let skip = if index == active_index + 1 {
                GROUP_THUMB_SKIP_CURRENT
            } else {
                GROUP_THUMB_SKIP
            };
            let left = prev_right + GROUP_THUMB_SKIP + skip;
            slots.push(ThumbSlot {
                index,
                rect: Rect::from_min_size(Pos2::new(left, y), size(GROUP_THUMB_WIDTH)),
                active: false,
            });
            prev_right = left + GROUP_THUMB_WIDTH;
        }

        slots.sort_by_key(|s| s.index);
        slots
    }

    /// Paints the strip inside `strip_rect` and returns the clicked item index, if any
    /// (`GroupThumbs::paint`, `Thumb::paint`). Items whose picture is not decoded yet get a
    /// plain dark tile.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        strip_rect: Rect,
        items: &[MediaItem],
        active_index: usize,
    ) -> Option<usize> {
        self.last_slots = Self::layout_slots(items, active_index, strip_rect);
        if self.last_slots.is_empty() {
            return None;
        }

        let mut clicked = None;
        let painter = ui.painter().clone();
        for slot in &self.last_slots {
            let item = &items[slot.index];
            let opacity = if slot.active { 1.0 } else { ALIVE_OPACITY };
            let tint = Color32::WHITE.gamma_multiply(opacity);
            match self.thumbs.get(&item.id) {
                Some(thumb) => {
                    // Narrower slots show the middle of the full thumbnail.
                    let frac = (slot.rect.width() / thumb.full_width).clamp(0.0, 1.0);
                    let uv = Rect::from_min_max(
                        Pos2::new(0.5 - frac / 2.0, 0.0),
                        Pos2::new(0.5 + frac / 2.0, 1.0),
                    );
                    painter.image(thumb.texture.id(), slot.rect, uv, tint);
                }
                None => {
                    painter.rect_filled(slot.rect, 0.0, MEDIAVIEW_MENU_BG.gamma_multiply(opacity));
                }
            }
            if slot.active {
                continue;
            }
            let resp = ui.interact(
                slot.rect,
                ui.scope_id().with(("mv_group_thumb", item.id.0)),
                Sense::click(),
            );
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            if resp.clicked() {
                clicked = Some(slot.index);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::backend::MediaItemKind;
    use tg_text::TextWithEntities;

    fn item(id: u64, width: u32, height: u32) -> MediaItem {
        MediaItem {
            id: MediaItemId(id),
            peer_id: 1,
            msg_id: id as i32,
            kind: MediaItemKind::Photo,
            width,
            height,
            duration_ms: 0,
            file_size: 1,
            file_name: String::new(),
            mime_type: String::new(),
            sender_name: String::new(),
            date_text: String::new(),
            caption: TextWithEntities::default(),
            rotation_degrees: 0,
            has_alpha: false,
            allows_forward: true,
            allows_delete: true,
            allows_save: true,
            local_path: None,
            attached_stickers_media: None,
            grouped_id: Some(7),
        }
    }

    #[test]
    fn strip_uses_tdesktop_sizes() {
        assert!((GroupThumbsStrip::height() - 108.0).abs() < f32::EPSILON);
        assert!((GROUP_THUMB_WIDTH - 56.0).abs() < f32::EPSILON);
        assert!((GROUP_THUMB_WIDTH_MAX - 160.0).abs() < f32::EPSILON);
        assert!((GROUP_THUMB_HEIGHT - 80.0).abs() < f32::EPSILON);
    }

    #[test]
    fn current_thumb_is_centered_at_its_aspect_width() {
        let strip = Rect::from_min_size(Pos2::new(0.0, 500.0), Vec2::new(600.0, 108.0));
        let items: Vec<_> = (0..5).map(|i| item(i, 800, 520)).collect();
        assert!(GroupThumbsStrip::layout_slots(&items[..1], 0, strip).is_empty());

        let slots = GroupThumbsStrip::layout_slots(&items, 2, strip);
        assert_eq!(slots.len(), 5);
        let current = slots[2];
        assert!(current.active);
        // 800×520 at 80 px tall is 123 px wide.
        assert!((current.rect.width() - 123.0).abs() < f32::EPSILON);
        assert!((current.rect.center().x - 300.0).abs() <= 1.0);
        assert!((current.rect.top() - 514.0).abs() < f32::EPSILON);
        assert!((slots[1].rect.width() - 56.0).abs() < f32::EPSILON);
        // 12 + 3 px between the current item and its neighbours, 3 + 3 px between others.
        assert!((current.rect.left() - slots[1].rect.right() - 15.0).abs() < f32::EPSILON);
        assert!((slots[1].rect.left() - slots[0].rect.right() - 6.0).abs() < f32::EPSILON);
        assert!((slots[3].rect.left() - current.rect.right() - 15.0).abs() < f32::EPSILON);

        // A panorama is capped at 160 px.
        let wide = vec![item(1, 4000, 500), item(2, 800, 520)];
        let slots = GroupThumbsStrip::layout_slots(&wide, 0, strip);
        assert!((slots[0].rect.width() - 160.0).abs() < f32::EPSILON);
    }

    #[test]
    fn strip_shows_at_most_the_limit_on_each_side() {
        assert_eq!(thumbs_limit(300.0), 1);
        assert_eq!(thumbs_limit(1000.0), 5);
        let strip = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(1000.0, 108.0));
        let items: Vec<_> = (0..25).map(|i| item(i, 800, 520)).collect();
        let slots = GroupThumbsStrip::layout_slots(&items, 12, strip);
        assert_eq!(slots.len(), 11);
        assert_eq!(slots.first().map(|s| s.index), Some(7));
        assert_eq!(slots.last().map(|s| s.index), Some(17));
    }
}
