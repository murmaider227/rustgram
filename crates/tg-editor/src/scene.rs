//! Paint scene model: brush lines, stickers, z-order, undo/redo, checkpoints, and rasterization.
//!
//! Port of `editor/scene/scene.h`, `editor/scene/scene.cpp`,
//! `editor/scene/scene_item_base.h/cpp`, `editor/scene/scene_item_line.h/cpp`,
//! `editor/scene/scene_item_sticker.h/cpp`, and `editor/scene/scene_draft.h/cpp`.

use std::collections::HashMap;

use image::RgbaImage;
use serde::{Deserialize, Serialize};
use tiny_skia::{
    BlendMode, ColorU8, FilterQuality, Paint, Pixmap, PixmapPaint, PremultipliedColorU8, Transform,
};

use crate::blur::{BLUR_BRUSH_RADIUS, stack_blur_rgba};
use crate::brush::{BrushStroke, rasterize_stroke, rasterize_stroke_mask};
use crate::common::{BrushTool, SaveState};
use crate::geometry::{Affine2D, PointF, RectF, Size, SizeF};
use crate::scene_items::{
    LocationItem, ShapeItem, StoryChannelPostItem, StoryLinkItem, StoryReactionItem, TextItem,
    WeatherItem, render_pill_onto, render_shape_onto, render_text_onto,
};

/// Starting z-value for scene items (`kItemsBaseZ` in `scene.cpp`).
pub const ITEMS_BASE_Z: f64 = 9000.0;

/// Rotation snap angle in degrees when Shift is held (`kSnapAngle` in `scene_item_base.cpp`).
pub const ITEM_SNAP_ANGLE: f64 = 45.0;

/// Minimum item size relative to max image side (`kMinSizeRatio` in `scene_item_base.cpp`).
pub const ITEM_MIN_SIZE_RATIO: f64 = 0.05;

/// Handle diameter in logical pixels (`photoEditorItemHandleSize`).
pub const ITEM_HANDLE_SIZE: f64 = 10.0;

/// Sticky guide snap trigger distance in logical pixels (`photoEditorStickyTrigger`).
pub const ITEM_STICKY_TRIGGER: f64 = 12.0;

/// Number of sticky alignment lines across each canvas axis (`kStickyLines`).
pub const STICKY_LINES: usize = 5;

/// Unique identifier for an item or action inside a [`PaintScene`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ItemId(pub u64);

/// Lifecycle status of a scene entry (`NumberedItem::Status`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ItemStatus {
    /// Active and visible.
    #[default]
    Normal,
    /// Undone (eligible for redo).
    Undid,
    /// Removed / discarded from redo history.
    Removed,
}

/// Transform and layout placement for a movable/scalable item (`ItemBase::Placement`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ItemPlacement {
    /// Center position in scene coordinates.
    pub position: PointF,
    /// Clockwise rotation in degrees.
    #[serde(default)]
    pub rotation: f64,
    /// Uniform scale factor (`1.0` = unscaled).
    #[serde(default = "default_one")]
    pub scale: f64,
    /// Stacking order (`zValue`).
    #[serde(default)]
    pub z_value: f64,
    /// Horizontal size in local scene units (`_horizontalSize`).
    #[serde(default)]
    pub size: f64,
    /// Aspect ratio (`height / width`, default `1.0`).
    #[serde(default = "default_one")]
    pub aspect_ratio: f64,
    /// Whether the item is mirrored horizontally.
    #[serde(default)]
    pub flipped: bool,
}

const fn default_one() -> f64 {
    1.0
}

impl Default for ItemPlacement {
    fn default() -> Self {
        Self {
            position: PointF::default(),
            rotation: 0.0,
            scale: 1.0,
            z_value: 0.0,
            size: 100.0,
            aspect_ratio: 1.0,
            flipped: false,
        }
    }
}

impl ItemPlacement {
    /// Horizontal size in local coordinates.
    #[must_use]
    pub const fn horizontal_size(&self) -> f64 {
        self.size
    }

    /// Vertical size in local coordinates (`size * aspect_ratio`).
    #[must_use]
    pub fn vertical_size(&self) -> f64 {
        self.size * self.aspect_ratio
    }

    /// Local inner rect centered at `(0, 0)` (`ItemBase::innerRect`).
    #[must_use]
    pub fn inner_rect(&self) -> RectF {
        let h = self.horizontal_size();
        let v = self.vertical_size();
        RectF::new(-h / 2.0, -v / 2.0, h, v)
    }

    /// Local rect for an image of `image_size` fitted inside `inner_rect` keeping aspect ratio
    /// (`ItemBase::fittedRect`).
    #[must_use]
    pub fn fitted_rect(&self, image_size: SizeF) -> RectF {
        let rect = self.inner_rect();
        if image_size.is_empty() {
            return rect;
        }
        let fitted = image_size.scaled_keep_aspect(rect.size());
        RectF::new(
            rect.x + (rect.width - fitted.width) / 2.0,
            rect.y + (rect.height - fitted.height) / 2.0,
            fitted.width,
            fitted.height,
        )
    }

    /// Left resize/rotate handle rectangle in local coordinates (`ItemBase::leftHandleRect`).
    #[must_use]
    pub fn left_handle_rect(&self, zoom: f64) -> RectF {
        let hs = ITEM_HANDLE_SIZE / zoom.max(1e-6);
        RectF::new(-self.size / 2.0 - hs / 2.0, -hs / 2.0, hs, hs)
    }

    /// Right resize/rotate handle rectangle in local coordinates (`ItemBase::rightHandleRect`).
    #[must_use]
    pub fn right_handle_rect(&self, zoom: f64) -> RectF {
        let hs = ITEM_HANDLE_SIZE / zoom.max(1e-6);
        RectF::new(self.size / 2.0 - hs / 2.0, -hs / 2.0, hs, hs)
    }

    /// Scene transform for this item (scale, rotate, translate to `position`).
    #[must_use]
    pub fn scene_transform(&self) -> Affine2D {
        Affine2D::from_scale(self.scale, self.scale)
            .then(Affine2D::from_rotate_deg(self.rotation))
            .then(Affine2D::from_translate(self.position.x, self.position.y))
    }

    /// Computes axis-aligned bounding box in scene coordinates.
    #[must_use]
    pub fn scene_bounding_rect(&self) -> RectF {
        self.scene_transform().map_rect(self.inner_rect())
    }

    /// Computes sticky snap shift on one axis (`ItemBase::computeSticky`).
    ///
    /// Returns `Some((snap_delta, guide_line_pos))` if the item is within
    /// `ITEM_STICKY_TRIGGER / zoom` of one of the 5 canvas alignment lines (`0%, 25%, 50%, 75%, 100%`).
    #[must_use]
    pub fn compute_sticky_axis(
        &self,
        canvas: RectF,
        horizontal: bool,
        zoom: f64,
    ) -> Option<(f64, f64)> {
        let trigger = ITEM_STICKY_TRIGGER / zoom.max(1e-6);
        let bounds = self.scene_bounding_rect();
        let (b_start, b_len, c_start, c_len) = if horizontal {
            (bounds.left(), bounds.width, canvas.left(), canvas.width)
        } else {
            (bounds.top(), bounds.height, canvas.top(), canvas.height)
        };

        let mut candidates: Vec<(i32, f64, f64)> = Vec::with_capacity(STICKY_LINES * 3);
        for line in 0..STICKY_LINES {
            let line_pos = c_start + c_len * (line as f64) / ((STICKY_LINES - 1) as f64);
            let line_prio = if line * 2 == STICKY_LINES - 1 {
                0
            } else if line == 0 || line == STICKY_LINES - 1 {
                1
            } else {
                2
            };
            for (anchor_idx, anchor_pos) in [
                (1, b_start),
                (0, b_start + b_len / 2.0),
                (1, b_start + b_len),
            ] {
                let prio = anchor_idx * 3 + line_prio;
                candidates.push((prio, line_pos - anchor_pos, line_pos));
            }
        }
        candidates.sort_by_key(|c| c.0);

        let mut kept: Vec<f64> = Vec::new();
        let mut best: Option<(f64, f64)> = None;
        let mut best_abs = trigger;
        for (_, shift, line_pos) in candidates {
            if shift.abs() > trigger {
                continue;
            }
            if kept.iter().any(|&k| (k - shift).abs() < trigger * 2.0) {
                continue;
            }
            kept.push(shift);
            if shift.abs() <= best_abs {
                best_abs = shift.abs();
                best = Some((shift, line_pos));
            }
        }
        best
    }
}

/// External sticker metadata (`SceneItemSticker` in `scene_draft.h`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StickerRef {
    /// Telegram sticker document ID.
    pub document_id: u64,
    /// Optional sticker set short name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub set_short_name: String,
    /// Optional sticker set ID.
    #[serde(default)]
    pub set_id: u64,
    /// Optional sticker set access hash.
    #[serde(default)]
    pub set_access_hash: u64,
}

/// Placed sticker layer on a [`PaintScene`] (`Editor::ItemSticker`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StickerItem {
    /// Sticker reference metadata (persisted in drafts).
    pub sticker: StickerRef,
    /// Position, scale, rotation, flip, size, and z-order.
    pub placement: ItemPlacement,
    /// Optional inline RGBA pixel layer (not serialized in drafts; can also be
    /// provided externally at render time via a resolver closure).
    #[serde(skip)]
    pub image: Option<RgbaImage>,
}

impl StickerItem {
    /// Creates a new sticker item with an externally provided RGBA image.
    ///
    /// Automatically computes `placement.aspect_ratio` from the image dimensions.
    #[must_use]
    pub fn with_image(sticker: StickerRef, mut placement: ItemPlacement, image: RgbaImage) -> Self {
        if image.width() > 0 && image.height() > 0 {
            placement.aspect_ratio = image.height() as f64 / image.width() as f64;
        }
        Self {
            sticker,
            placement,
            image: Some(image),
        }
    }
}

/// Brush line layer on a [`PaintScene`] (`Editor::ItemLine`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LineItem {
    /// Vector brush stroke definition.
    pub stroke: BrushStroke,
    /// Stacking order (`zValue`).
    pub z_value: f64,
}

/// Undoable eraser stroke targeting existing [`LineItem`]s (`ItemEraser`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EraserAction {
    /// Eraser stroke path/width.
    pub stroke: BrushStroke,
    /// IDs of [`LineItem`]s intersected at the time the eraser was applied.
    pub target_ids: Vec<ItemId>,
}

/// Undoable placement change on a movable item (`ItemPlacement` action).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlacementAction {
    /// Target item ID.
    pub target_id: ItemId,
    /// Placement before the edit.
    pub before: ItemPlacement,
    /// Placement after the edit.
    pub after: ItemPlacement,
}

/// Undoable z-order swap (`ItemZOrder` action).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZOrderAction {
    /// List of `(item_id, z_before, z_after)` mutations.
    pub targets: Vec<(ItemId, f64, f64)>,
}

/// Undoable item deletion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoveAction {
    /// Target item ID.
    pub target_id: ItemId,
}

/// Variant stored inside a [`SceneEntry`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SceneItemKind {
    /// Freehand brush/arrow/marker/blur stroke.
    Line(LineItem),
    /// Placed sticker layer.
    Sticker(StickerItem),
    /// Styled text overlay (`Editor::ItemText`).
    Text(TextItem),
    /// Vector shape overlay (`Editor::ItemShape`).
    Shape(ShapeItem),
    /// Interactive geo-location / venue pill (`Editor::ItemLocation`).
    Location(LocationItem),
    /// Interactive weather pill (`Editor::ItemWeather`).
    Weather(WeatherItem),
    /// Clickable story URL link pill (`Editor::ItemLink`).
    StoryLink(StoryLinkItem),
    /// Interactive story reaction sticker (`Editor::ItemReaction`).
    StoryReaction(StoryReactionItem),
    /// Reposted channel message story card (`Editor::ItemMessage`).
    StoryChannelPost(StoryChannelPostItem),
    /// Eraser stroke affecting earlier [`SceneItemKind::Line`] items.
    Eraser(EraserAction),
    /// Transform/placement edit on an item.
    Placement(PlacementAction),
    /// Z-order raise/lower swap.
    ZOrder(ZOrderAction),
    /// Deletion of an item.
    Remove(RemoveAction),
}

impl SceneItemKind {
    /// Returns true if this entry is a history action rather than a drawable item.
    #[must_use]
    pub const fn is_action(&self) -> bool {
        matches!(
            self,
            Self::Eraser(_) | Self::Placement(_) | Self::ZOrder(_) | Self::Remove(_)
        )
    }
}

/// Numbered timeline entry in a [`PaintScene`] (`NumberedItem`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneEntry {
    /// Unique identifier.
    pub id: ItemId,
    /// Monotonic insertion sequence number (`NumberedItem::number`).
    pub number: u64,
    /// Current undo/redo status.
    pub status: ItemStatus,
    /// Whether this entry can be undone (`NumberedItem::undoable`).
    #[serde(default = "default_true")]
    pub undoable: bool,
    /// Payload.
    pub item: SceneItemKind,
}

const fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Default)]
struct SceneCheckpoint {
    entries: Vec<SceneEntry>,
    last_z: f64,
    next_id: u64,
    next_number: u64,
}

/// Paint scene holding strokes, stickers, z-order, undo/redo history, and checkpoints
/// (`Editor::Scene`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaintScene {
    /// Full image bounds in scene coordinates.
    pub scene_rect: RectF,
    /// Active canvas bounds (expanded crop united with image rect).
    #[serde(default)]
    pub canvas_rect: RectF,
    entries: Vec<SceneEntry>,
    last_z: f64,
    next_id: u64,
    next_number: u64,
    #[serde(skip)]
    kept: Option<SceneCheckpoint>,
    #[serde(skip)]
    saved: Option<SceneCheckpoint>,
}

impl Default for PaintScene {
    fn default() -> Self {
        Self::new(RectF::default())
    }
}

impl PaintScene {
    /// Creates an empty scene covering `scene_rect`.
    #[must_use]
    pub fn new(scene_rect: RectF) -> Self {
        Self {
            scene_rect,
            canvas_rect: scene_rect,
            entries: Vec::new(),
            last_z: ITEMS_BASE_Z,
            next_id: 1,
            next_number: 0,
            kept: None,
            saved: None,
        }
    }

    /// Creates an empty scene sized to `size` at `(0, 0)`.
    #[must_use]
    pub fn from_size(size: Size) -> Self {
        Self::new(RectF::new(0.0, 0.0, size.width as f64, size.height as f64))
    }

    /// Sets the active canvas rect (`Scene::setCanvasRect`).
    pub fn set_canvas_rect(&mut self, rect: RectF) {
        self.canvas_rect = rect;
    }

    /// Returns the active canvas rect, falling back to `scene_rect`.
    #[must_use]
    pub fn active_canvas_rect(&self) -> RectF {
        if self.canvas_rect.is_empty() {
            self.scene_rect
        } else {
            self.canvas_rect
        }
    }

    /// Returns all entries in insertion order.
    #[must_use]
    pub fn entries(&self) -> &[SceneEntry] {
        &self.entries
    }

    /// Returns true if any drawable item is currently `Normal`.
    #[must_use]
    pub fn has_active_items(&self) -> bool {
        self.entries
            .iter()
            .any(|e| e.status == ItemStatus::Normal && !e.item.is_action())
    }

    /// Looks up an entry by [`ItemId`].
    #[must_use]
    pub fn get(&self, id: ItemId) -> Option<&SceneEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Adds a brush stroke (`Pen`, `Arrow`, `Marker`, `Blur`, or `Eraser`).
    ///
    /// - If `stroke.points.len() < 2`, returns `None` (no-op).
    /// - If `stroke.tool == BrushTool::Eraser`, finds all active `Line` items whose
    ///   bounding rect intersects the eraser stroke and records an undoable [`EraserAction`].
    ///   Returns `None` if no line items were hit.
    /// - Clears any redo stack (`Undid` -> `Removed`) before adding.
    pub fn add_stroke(&mut self, stroke: BrushStroke) -> Option<ItemId> {
        if stroke.points.len() < 2 {
            return None;
        }
        if stroke.tool == BrushTool::Eraser {
            let mask_rect = stroke.bounding_rect();
            let mut target_ids = Vec::new();
            for entry in &self.entries {
                if entry.status != ItemStatus::Normal {
                    continue;
                }
                if let SceneItemKind::Line(line) = &entry.item
                    && line.stroke.bounding_rect().intersects(mask_rect)
                {
                    target_ids.push(entry.id);
                }
            }
            if target_ids.is_empty() {
                return None;
            }
            self.clear_redo_list();
            let id = self.push_entry(SceneItemKind::Eraser(EraserAction { stroke, target_ids }));
            return Some(id);
        }

        self.clear_redo_list();
        let z = self.alloc_z();
        let id = self.push_entry(SceneItemKind::Line(LineItem { stroke, z_value: z }));
        Some(id)
    }

    /// Adds a sticker layer to the scene, assigning the next top z-value if `placement.z_value <= 0`.
    pub fn add_sticker(&mut self, mut sticker: StickerItem) -> ItemId {
        self.clear_redo_list();
        self.assign_placement_z(&mut sticker.placement);
        self.push_entry(SceneItemKind::Sticker(sticker))
    }

    /// Adds a text overlay layer (`Editor::ItemText`) to the scene.
    pub fn add_text(&mut self, mut item: TextItem) -> ItemId {
        self.clear_redo_list();
        self.assign_placement_z(&mut item.placement);
        self.push_entry(SceneItemKind::Text(item))
    }

    /// Adds a vector shape overlay (`Editor::ItemShape`) to the scene.
    pub fn add_shape(&mut self, mut item: ShapeItem) -> ItemId {
        self.clear_redo_list();
        self.assign_placement_z(&mut item.placement);
        self.push_entry(SceneItemKind::Shape(item))
    }

    /// Adds a location pill (`Editor::ItemLocation`) to the scene.
    pub fn add_location(&mut self, mut item: LocationItem) -> ItemId {
        self.clear_redo_list();
        self.assign_placement_z(&mut item.placement);
        self.push_entry(SceneItemKind::Location(item))
    }

    /// Adds a weather pill (`Editor::ItemWeather`) to the scene.
    pub fn add_weather(&mut self, mut item: WeatherItem) -> ItemId {
        self.clear_redo_list();
        self.assign_placement_z(&mut item.placement);
        self.push_entry(SceneItemKind::Weather(item))
    }

    /// Adds a clickable story link pill (`Editor::ItemLink`) to the scene.
    pub fn add_story_link(&mut self, mut item: StoryLinkItem) -> ItemId {
        self.clear_redo_list();
        self.assign_placement_z(&mut item.placement);
        self.push_entry(SceneItemKind::StoryLink(item))
    }

    /// Adds an interactive story reaction area (`Editor::ItemReaction`) to the scene.
    pub fn add_story_reaction(&mut self, mut item: StoryReactionItem) -> ItemId {
        self.clear_redo_list();
        self.assign_placement_z(&mut item.placement);
        self.push_entry(SceneItemKind::StoryReaction(item))
    }

    /// Adds a story channel-post card (`Editor::ItemMessage`) to the scene.
    pub fn add_story_channel_post(&mut self, mut item: StoryChannelPostItem) -> ItemId {
        self.clear_redo_list();
        self.assign_placement_z(&mut item.placement);
        self.push_entry(SceneItemKind::StoryChannelPost(item))
    }

    /// Returns the current [`ItemPlacement`] of any movable scene item (`Sticker`, `Text`,
    /// `Shape`, `Location`, `Weather`, or story area).
    #[must_use]
    pub fn item_placement(&self, id: ItemId) -> Option<ItemPlacement> {
        self.entries.iter().find_map(|e| {
            if e.id == id && e.status == ItemStatus::Normal {
                entry_placement(&e.item)
            } else {
                None
            }
        })
    }

    /// Updates any movable scene item's placement and records an undoable [`PlacementAction`].
    pub fn set_item_placement(&mut self, id: ItemId, placement: ItemPlacement) -> bool {
        let Some(before) = self.item_placement(id) else {
            return false;
        };
        if before == placement {
            return false;
        }
        self.clear_redo_list();
        self.apply_placement_raw(id, placement);
        self.push_entry(SceneItemKind::Placement(PlacementAction {
            target_id: id,
            before,
            after: placement,
        }));
        true
    }

    /// Updates a sticker's placement and records an undoable [`PlacementAction`].
    ///
    /// Returns `true` if the item existed and its placement changed.
    pub fn set_sticker_placement(&mut self, id: ItemId, placement: ItemPlacement) -> bool {
        self.set_item_placement(id, placement)
    }

    /// Toggles horizontal flip on a sticker item (recorded in undo history).
    pub fn flip_sticker(&mut self, id: ItemId) -> bool {
        let Some(mut placement) = self.entries.iter().find_map(|e| {
            if e.id == id
                && e.status == ItemStatus::Normal
                && let SceneItemKind::Sticker(s) = &e.item
            {
                return Some(s.placement);
            }
            None
        }) else {
            return false;
        };
        placement.flipped = !placement.flipped;
        self.set_sticker_placement(id, placement)
    }

    /// Duplicates a sticker item offset by `(horizontal_size / 3, vertical_size / 3)`
    /// (`ItemBase::actionDuplicate`).
    pub fn duplicate_sticker(&mut self, id: ItemId) -> Option<ItemId> {
        let mut copy = self.entries.iter().find_map(|e| {
            if e.id == id
                && e.status == ItemStatus::Normal
                && let SceneItemKind::Sticker(s) = &e.item
            {
                return Some(s.clone());
            }
            None
        })?;
        copy.placement.position.x += (copy.placement.horizontal_size() / 3.0).trunc();
        copy.placement.position.y += (copy.placement.vertical_size() / 3.0).trunc();
        copy.placement.z_value = 0.0;
        Some(self.add_sticker(copy))
    }

    /// Removes an active drawable item (`Scene::removeItem`).
    pub fn remove_item(&mut self, id: ItemId) -> bool {
        let exists = self
            .entries
            .iter()
            .any(|e| e.id == id && e.status == ItemStatus::Normal && !e.item.is_action());
        if !exists {
            return false;
        }
        self.clear_redo_list();
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
            entry.status = ItemStatus::Removed;
        }
        self.push_entry(SceneItemKind::Remove(RemoveAction { target_id: id }));
        true
    }

    /// Raises an item one step in z-order by swapping with the nearest item above it
    /// (`Scene::raiseItem`).
    pub fn raise_item(&mut self, id: ItemId) -> bool {
        self.change_item_z_order(id, 1)
    }

    /// Lowers an item one step in z-order by swapping with the nearest item below it
    /// (`Scene::lowerItem`).
    pub fn lower_item(&mut self, id: ItemId) -> bool {
        self.change_item_z_order(id, -1)
    }

    /// Pins an item to the bottom z-layer (`Scene::pinToBottom`).
    pub fn pin_to_bottom(&mut self, id: ItemId) -> bool {
        let Some(before) = self.item_z(id) else {
            return false;
        };
        let target_z = ITEMS_BASE_Z - 1.0;
        if (before - target_z).abs() < f64::EPSILON {
            return false;
        }
        self.clear_redo_list();
        self.set_item_z_raw(id, target_z);
        self.push_entry(SceneItemKind::ZOrder(ZOrderAction {
            targets: vec![(id, before, target_z)],
        }));
        true
    }

    /// Returns true if at least one active entry can be undone (`Scene::hasUndo`).
    #[must_use]
    pub fn has_undo(&self) -> bool {
        self.entries
            .iter()
            .any(|e| e.status == ItemStatus::Normal && e.undoable)
    }

    /// Returns true if at least one entry can be redone (`Scene::hasRedo`).
    #[must_use]
    pub fn has_redo(&self) -> bool {
        self.entries.iter().any(|e| e.status == ItemStatus::Undid)
    }

    /// Undoes the most recent active entry (`Scene::performUndo`).
    pub fn undo(&mut self) -> bool {
        let Some(idx) = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.status == ItemStatus::Normal && e.undoable)
            .max_by_key(|(_, e)| e.number)
            .map(|(i, _)| i)
        else {
            return false;
        };

        let action = self.entries[idx].item.clone();
        self.entries[idx].status = ItemStatus::Undid;
        match action {
            SceneItemKind::Placement(p) => self.apply_placement_raw(p.target_id, p.before),
            SceneItemKind::ZOrder(z) => {
                for (target, before, _) in z.targets {
                    self.set_item_z_raw(target, before);
                }
            }
            SceneItemKind::Remove(r) => {
                if let Some(target) = self.entries.iter_mut().find(|e| e.id == r.target_id) {
                    target.status = ItemStatus::Normal;
                }
            }
            SceneItemKind::Eraser(_)
            | SceneItemKind::Line(_)
            | SceneItemKind::Sticker(_)
            | SceneItemKind::Text(_)
            | SceneItemKind::Shape(_)
            | SceneItemKind::Location(_)
            | SceneItemKind::Weather(_)
            | SceneItemKind::StoryLink(_)
            | SceneItemKind::StoryReaction(_)
            | SceneItemKind::StoryChannelPost(_) => {}
        }
        true
    }

    /// Redoes the oldest undone entry (`Scene::performRedo`).
    pub fn redo(&mut self) -> bool {
        let Some(idx) = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.status == ItemStatus::Undid)
            .min_by_key(|(_, e)| e.number)
            .map(|(i, _)| i)
        else {
            return false;
        };

        let action = self.entries[idx].item.clone();
        self.entries[idx].status = ItemStatus::Normal;
        match action {
            SceneItemKind::Placement(p) => self.apply_placement_raw(p.target_id, p.after),
            SceneItemKind::ZOrder(z) => {
                for (target, _, after) in z.targets {
                    self.set_item_z_raw(target, after);
                }
            }
            SceneItemKind::Remove(r) => {
                if let Some(target) = self.entries.iter_mut().find(|e| e.id == r.target_id) {
                    target.status = ItemStatus::Removed;
                }
            }
            SceneItemKind::Eraser(_)
            | SceneItemKind::Line(_)
            | SceneItemKind::Sticker(_)
            | SceneItemKind::Text(_)
            | SceneItemKind::Shape(_)
            | SceneItemKind::Location(_)
            | SceneItemKind::Weather(_)
            | SceneItemKind::StoryLink(_)
            | SceneItemKind::StoryReaction(_)
            | SceneItemKind::StoryChannelPost(_) => {}
        }
        true
    }

    /// Marks all `Undid` entries as `Removed` (`Scene::clearRedoList`).
    pub fn clear_redo_list(&mut self) {
        for entry in &mut self.entries {
            if entry.status == ItemStatus::Undid {
                entry.status = ItemStatus::Removed;
            }
        }
    }

    /// Saves a checkpoint for paint-mode (`SaveState::Keep`) or whole-editor (`SaveState::Save`)
    /// cancel/restore (`Scene::save`).
    pub fn save(&mut self, state: SaveState) {
        let cp = SceneCheckpoint {
            entries: self.entries.clone(),
            last_z: self.last_z,
            next_id: self.next_id,
            next_number: self.next_number,
        };
        match state {
            SaveState::Keep => self.kept = Some(cp),
            SaveState::Save => self.saved = Some(cp),
        }
    }

    /// Restores a previously saved checkpoint (`Scene::restore`).
    pub fn restore(&mut self, state: SaveState) -> bool {
        let cp = match state {
            SaveState::Keep => self.kept.clone(),
            SaveState::Save => self.saved.clone(),
        };
        let Some(cp) = cp else {
            return false;
        };
        self.entries = cp.entries;
        self.last_z = cp.last_z;
        self.next_id = cp.next_id;
        self.next_number = cp.next_number;
        true
    }

    /// Returns all active drawable items stably sorted by ascending `z_value`.
    #[must_use]
    pub fn active_items_by_z(&self) -> Vec<&SceneEntry> {
        let mut items: Vec<&SceneEntry> = self
            .entries
            .iter()
            .filter(|e| e.status == ItemStatus::Normal && !e.item.is_action())
            .collect();
        items.sort_by(|a, b| {
            let za = entry_z(a);
            let zb = entry_z(b);
            za.total_cmp(&zb).then_with(|| a.number.cmp(&b.number))
        });
        items
    }

    /// Returns IDs of all active sticker documents in z-order (for Telegram attachedStickers).
    #[must_use]
    pub fn active_sticker_refs(&self) -> Vec<StickerRef> {
        self.active_items_by_z()
            .into_iter()
            .filter_map(|e| match &e.item {
                SceneItemKind::Sticker(s) => Some(s.sticker.clone()),
                _ => None,
            })
            .collect()
    }

    /// Computes a deterministic 64-bit FNV-1a content fingerprint of all active items
    /// (analog of `SceneFingerprint` in `scene_draft.cpp`).
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let feed = |h: &mut u64, bytes: &[u8]| {
            for &b in bytes {
                *h ^= u64::from(b);
                *h = h.wrapping_mul(0x0100_0000_01b3);
            }
        };
        for entry in &self.entries {
            if entry.status != ItemStatus::Normal {
                continue;
            }
            match &entry.item {
                SceneItemKind::Line(line) => {
                    feed(&mut h, b"line");
                    feed(&mut h, &[line.stroke.tool as u8]);
                    feed(&mut h, &line.stroke.color.to_argb_u32().to_le_bytes());
                    feed(&mut h, &line.stroke.size.to_bits().to_le_bytes());
                    feed(&mut h, &line.z_value.to_bits().to_le_bytes());
                    for pt in &line.stroke.points {
                        feed(&mut h, &pt.pos.x.to_bits().to_le_bytes());
                        feed(&mut h, &pt.pos.y.to_bits().to_le_bytes());
                        feed(&mut h, &pt.pressure.to_bits().to_le_bytes());
                    }
                }
                SceneItemKind::Sticker(sticker) => {
                    feed(&mut h, b"sticker");
                    feed(&mut h, &sticker.sticker.document_id.to_le_bytes());
                    feed_placement(&mut h, &sticker.placement);
                }
                SceneItemKind::Text(text) => {
                    feed(&mut h, b"text");
                    feed(&mut h, text.text.as_bytes());
                    feed(&mut h, &text.color.to_argb_u32().to_le_bytes());
                    feed(&mut h, &text.font_size.to_bits().to_le_bytes());
                    feed(
                        &mut h,
                        &[text.style as u8, text.typeface as u8, text.alignment as u8],
                    );
                    feed_placement(&mut h, &text.placement);
                }
                SceneItemKind::Shape(shape) => {
                    feed(&mut h, b"shape");
                    feed(&mut h, &[shape.shape as u8, u8::from(shape.fill)]);
                    feed(&mut h, &shape.color.to_argb_u32().to_le_bytes());
                    feed(&mut h, &shape.stroke_width.to_bits().to_le_bytes());
                    feed(&mut h, &shape.bend.to_bits().to_le_bytes());
                    feed_placement(&mut h, &shape.placement);
                }
                SceneItemKind::Location(loc) => {
                    feed(&mut h, b"location");
                    feed(&mut h, &loc.location.lat.to_bits().to_le_bytes());
                    feed(&mut h, &loc.location.lon.to_bits().to_le_bytes());
                    feed(&mut h, loc.location.title.as_bytes());
                    feed_placement(&mut h, &loc.placement);
                }
                SceneItemKind::Weather(w) => {
                    feed(&mut h, b"weather");
                    feed(&mut h, w.weather.emoji.as_bytes());
                    feed(&mut h, &w.weather.celsius.to_bits().to_le_bytes());
                    feed(&mut h, &w.weather.color.to_argb_u32().to_le_bytes());
                    feed_placement(&mut h, &w.placement);
                }
                SceneItemKind::StoryLink(link) => {
                    feed(&mut h, b"story_link");
                    feed(&mut h, link.url.as_bytes());
                    feed(&mut h, link.title.as_bytes());
                    feed_placement(&mut h, &link.placement);
                }
                SceneItemKind::StoryReaction(rx) => {
                    feed(&mut h, b"story_reaction");
                    feed(&mut h, rx.reaction.as_bytes());
                    feed(&mut h, &[u8::from(rx.dark)]);
                    feed_placement(&mut h, &rx.placement);
                }
                SceneItemKind::StoryChannelPost(post) => {
                    feed(&mut h, b"story_post");
                    feed(&mut h, &post.channel_id.to_le_bytes());
                    feed(&mut h, &post.msg_id.to_le_bytes());
                    feed_placement(&mut h, &post.placement);
                }
                SceneItemKind::Eraser(eraser) => {
                    feed(&mut h, b"eraser");
                    feed(&mut h, &eraser.stroke.size.to_bits().to_le_bytes());
                    for tid in &eraser.target_ids {
                        feed(&mut h, &tid.0.to_le_bytes());
                    }
                    for pt in &eraser.stroke.points {
                        feed(&mut h, &pt.pos.x.to_bits().to_le_bytes());
                        feed(&mut h, &pt.pos.y.to_bits().to_le_bytes());
                    }
                }
                SceneItemKind::Placement(_)
                | SceneItemKind::ZOrder(_)
                | SceneItemKind::Remove(_) => {}
            }
        }
        h
    }

    /// Renders all active scene items in z-order into `target` (`Scene::renderBaked`).
    ///
    /// Maps `source_rect` in scene coordinates onto `target_rect` in `target` pixmap coordinates.
    /// External sticker images are resolved via `sticker_resolver` (falling back to the
    /// inline `StickerItem::image` if `sticker_resolver` returns `None`).
    pub fn render_baked<'a, F>(
        &self,
        target: &mut Pixmap,
        target_rect: RectF,
        source_rect: RectF,
        mut sticker_resolver: F,
    ) where
        F: FnMut(&StickerRef) -> Option<&'a RgbaImage>,
    {
        if target_rect.is_empty() || source_rect.is_empty() {
            return;
        }
        let sx = target_rect.width / source_rect.width;
        let sy = target_rect.height / source_rect.height;
        let scene_to_target = Affine2D::from_translate(-source_rect.x, -source_rect.y)
            .then(Affine2D::from_scale(sx, sy))
            .then(Affine2D::from_translate(target_rect.x, target_rect.y));
        let skia_scene_to_target = scene_to_target.to_skia();

        // Collect active eraser strokes per target line item.
        let mut erasers_for: HashMap<ItemId, Vec<&BrushStroke>> = HashMap::new();
        for entry in &self.entries {
            if entry.status != ItemStatus::Normal {
                continue;
            }
            if let SceneItemKind::Eraser(eraser) = &entry.item {
                for &tid in &eraser.target_ids {
                    erasers_for.entry(tid).or_default().push(&eraser.stroke);
                }
            }
        }

        let w = target.width();
        let h = target.height();
        let sorted = self.active_items_by_z();

        for entry in sorted {
            match &entry.item {
                SceneItemKind::Line(line) => {
                    let erasers = erasers_for.get(&entry.id);
                    if line.stroke.tool == BrushTool::Blur {
                        self.render_blur_line(
                            target,
                            &line.stroke,
                            skia_scene_to_target,
                            erasers.map(Vec::as_slice),
                        );
                    } else if let Some(erasers) = erasers {
                        if let Some(mut layer) = Pixmap::new(w, h) {
                            rasterize_stroke(&mut layer, &line.stroke, skia_scene_to_target);
                            for eraser_stroke in erasers {
                                if let Some(mask) =
                                    rasterize_stroke_mask(w, h, eraser_stroke, skia_scene_to_target)
                                {
                                    let erase_paint = PixmapPaint {
                                        blend_mode: BlendMode::DestinationOut,
                                        ..PixmapPaint::default()
                                    };
                                    layer.draw_pixmap(
                                        0,
                                        0,
                                        mask.as_ref(),
                                        &erase_paint,
                                        Transform::identity(),
                                        None,
                                    );
                                }
                            }
                            target.draw_pixmap(
                                0,
                                0,
                                layer.as_ref(),
                                &PixmapPaint::default(),
                                Transform::identity(),
                                None,
                            );
                        }
                    } else {
                        rasterize_stroke(target, &line.stroke, skia_scene_to_target);
                    }
                }
                SceneItemKind::Sticker(sticker) => {
                    let resolved = sticker_resolver(&sticker.sticker);
                    let img = resolved.or(sticker.image.as_ref());
                    if let Some(rgba) = img {
                        render_sticker_onto(target, rgba, &sticker.placement, scene_to_target);
                    }
                }
                SceneItemKind::Text(text) => {
                    render_text_onto(target, text, scene_to_target);
                }
                SceneItemKind::Shape(shape) => {
                    render_shape_onto(target, shape, scene_to_target);
                }
                SceneItemKind::Location(loc) => {
                    render_pill_onto(
                        target,
                        &loc.placement,
                        loc.style.background_color(),
                        loc.style.foreground_color(),
                        &loc.location.display_label(),
                        scene_to_target,
                    );
                }
                SceneItemKind::Weather(w) => {
                    render_pill_onto(
                        target,
                        &w.placement,
                        w.weather.color,
                        w.weather.foreground(),
                        &w.weather.formatted_celsius(),
                        scene_to_target,
                    );
                }
                SceneItemKind::StoryLink(link) => {
                    let lbl = if link.title.is_empty() {
                        link.url.as_str()
                    } else {
                        link.title.as_str()
                    };
                    render_pill_onto(
                        target,
                        &link.placement,
                        link.style.background_color(),
                        link.style.foreground_color(),
                        lbl,
                        scene_to_target,
                    );
                }
                SceneItemKind::StoryReaction(rx) => {
                    let bg = if rx.dark {
                        crate::geometry::Color::rgba(28, 32, 38, 225)
                    } else {
                        crate::geometry::Color::rgba(255, 255, 255, 235)
                    };
                    let fg = if rx.dark {
                        crate::geometry::Color::WHITE
                    } else {
                        crate::geometry::Color::BLACK
                    };
                    render_pill_onto(target, &rx.placement, bg, fg, &rx.reaction, scene_to_target);
                }
                SceneItemKind::StoryChannelPost(post) => {
                    let lbl = if post.preview_text.is_empty() {
                        "POST"
                    } else {
                        post.preview_text.as_str()
                    };
                    render_pill_onto(
                        target,
                        &post.placement,
                        crate::geometry::Color::rgba(24, 30, 40, 220),
                        crate::geometry::Color::WHITE,
                        lbl,
                        scene_to_target,
                    );
                }
                SceneItemKind::Eraser(_)
                | SceneItemKind::Placement(_)
                | SceneItemKind::ZOrder(_)
                | SceneItemKind::Remove(_) => {}
            }
        }
    }

    fn render_blur_line(
        &self,
        target: &mut Pixmap,
        stroke: &BrushStroke,
        scene_to_target: Transform,
        erasers: Option<&[&BrushStroke]>,
    ) {
        let w = target.width();
        let h = target.height();
        let Some(mut mask) = rasterize_stroke_mask(w, h, stroke, scene_to_target) else {
            return;
        };
        if let Some(erasers) = erasers {
            let erase_paint = PixmapPaint {
                blend_mode: BlendMode::DestinationOut,
                ..PixmapPaint::default()
            };
            for eraser_stroke in erasers {
                if let Some(erase_mask) =
                    rasterize_stroke_mask(w, h, eraser_stroke, scene_to_target)
                {
                    mask.draw_pixmap(
                        0,
                        0,
                        erase_mask.as_ref(),
                        &erase_paint,
                        Transform::identity(),
                        None,
                    );
                }
            }
        }
        let snapshot = pixmap_to_rgba(target);
        let blurred = stack_blur_rgba(&snapshot, BLUR_BRUSH_RADIUS);
        let Some(mut blurred_pixmap) = rgba_to_pixmap(&blurred) else {
            return;
        };
        let mask_paint = PixmapPaint {
            blend_mode: BlendMode::DestinationIn,
            ..PixmapPaint::default()
        };
        blurred_pixmap.draw_pixmap(
            0,
            0,
            mask.as_ref(),
            &mask_paint,
            Transform::identity(),
            None,
        );
        target.draw_pixmap(
            0,
            0,
            blurred_pixmap.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
    }

    fn alloc_z(&mut self) -> f64 {
        let z = self.last_z;
        self.last_z += 1.0;
        z
    }

    fn push_entry(&mut self, item: SceneItemKind) -> ItemId {
        let id = ItemId(self.next_id);
        self.next_id += 1;
        let number = self.next_number;
        self.next_number += 1;
        self.entries.push(SceneEntry {
            id,
            number,
            status: ItemStatus::Normal,
            undoable: true,
            item,
        });
        id
    }

    fn item_z(&self, id: ItemId) -> Option<f64> {
        self.entries.iter().find_map(|e| {
            if e.id == id && e.status == ItemStatus::Normal && !e.item.is_action() {
                Some(entry_z(e))
            } else {
                None
            }
        })
    }

    fn assign_placement_z(&mut self, placement: &mut ItemPlacement) {
        if placement.z_value <= 0.0 {
            placement.z_value = self.alloc_z();
        } else {
            self.last_z = self.last_z.max(placement.z_value + 1.0);
        }
    }

    fn set_item_z_raw(&mut self, id: ItemId, z: f64) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
            match &mut entry.item {
                SceneItemKind::Line(l) => l.z_value = z,
                SceneItemKind::Sticker(s) => s.placement.z_value = z,
                SceneItemKind::Text(t) => t.placement.z_value = z,
                SceneItemKind::Shape(s) => s.placement.z_value = z,
                SceneItemKind::Location(l) => l.placement.z_value = z,
                SceneItemKind::Weather(w) => w.placement.z_value = z,
                SceneItemKind::StoryLink(l) => l.placement.z_value = z,
                SceneItemKind::StoryReaction(r) => r.placement.z_value = z,
                SceneItemKind::StoryChannelPost(p) => p.placement.z_value = z,
                _ => {}
            }
        }
    }

    fn apply_placement_raw(&mut self, id: ItemId, placement: ItemPlacement) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
            match &mut entry.item {
                SceneItemKind::Sticker(s) => s.placement = placement,
                SceneItemKind::Text(t) => t.placement = placement,
                SceneItemKind::Shape(s) => s.placement = placement,
                SceneItemKind::Location(l) => l.placement = placement,
                SceneItemKind::Weather(w) => w.placement = placement,
                SceneItemKind::StoryLink(l) => l.placement = placement,
                SceneItemKind::StoryReaction(r) => r.placement = placement,
                SceneItemKind::StoryChannelPost(p) => p.placement = placement,
                _ => {}
            }
        }
    }

    fn change_item_z_order(&mut self, id: ItemId, step: i32) -> bool {
        let Some(z) = self.item_z(id) else {
            return false;
        };
        let mut best: Option<(ItemId, f64)> = None;
        for candidate in &self.entries {
            if candidate.id == id
                || candidate.status != ItemStatus::Normal
                || candidate.item.is_action()
            {
                continue;
            }
            let cz = entry_z(candidate);
            let closer = if step > 0 { cz > z } else { cz < z };
            if !closer {
                continue;
            }
            let replace = match best {
                None => true,
                Some((_, bz)) => {
                    if step > 0 {
                        cz < bz
                    } else {
                        cz > bz
                    }
                }
            };
            if replace {
                best = Some((candidate.id, cz));
            }
        }
        let Some((best_id, best_z)) = best else {
            return false;
        };
        self.clear_redo_list();
        self.set_item_z_raw(best_id, z);
        self.set_item_z_raw(id, best_z);
        self.push_entry(SceneItemKind::ZOrder(ZOrderAction {
            targets: vec![(id, z, best_z), (best_id, best_z, z)],
        }));
        true
    }
}

fn entry_z(entry: &SceneEntry) -> f64 {
    match &entry.item {
        SceneItemKind::Line(l) => l.z_value,
        other => entry_placement(other).map_or(0.0, |p| p.z_value),
    }
}

fn entry_placement(kind: &SceneItemKind) -> Option<ItemPlacement> {
    match kind {
        SceneItemKind::Sticker(s) => Some(s.placement),
        SceneItemKind::Text(t) => Some(t.placement),
        SceneItemKind::Shape(s) => Some(s.placement),
        SceneItemKind::Location(l) => Some(l.placement),
        SceneItemKind::Weather(w) => Some(w.placement),
        SceneItemKind::StoryLink(l) => Some(l.placement),
        SceneItemKind::StoryReaction(r) => Some(r.placement),
        SceneItemKind::StoryChannelPost(p) => Some(p.placement),
        _ => None,
    }
}

fn feed_placement(h: &mut u64, placement: &ItemPlacement) {
    let feed = |h: &mut u64, bytes: &[u8]| {
        for &b in bytes {
            *h ^= u64::from(b);
            *h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    feed(h, &placement.position.x.to_bits().to_le_bytes());
    feed(h, &placement.position.y.to_bits().to_le_bytes());
    feed(h, &placement.rotation.to_bits().to_le_bytes());
    feed(h, &placement.scale.to_bits().to_le_bytes());
    feed(h, &placement.z_value.to_bits().to_le_bytes());
    feed(h, &placement.size.to_bits().to_le_bytes());
    feed(h, &placement.aspect_ratio.to_bits().to_le_bytes());
    feed(h, &[u8::from(placement.flipped)]);
}

fn render_sticker_onto(
    target: &mut Pixmap,
    rgba: &RgbaImage,
    placement: &ItemPlacement,
    scene_to_target: Affine2D,
) {
    if rgba.width() == 0 || rgba.height() == 0 {
        return;
    }
    let Some(sticker_pixmap) = rgba_to_pixmap(rgba) else {
        return;
    };
    let img_w = rgba.width() as f64;
    let img_h = rgba.height() as f64;
    let fitted = placement.fitted_rect(SizeF::new(img_w, img_h));
    if fitted.is_empty() {
        return;
    }
    let sx = (fitted.width / img_w) * if placement.flipped { -1.0 } else { 1.0 };
    let sy = fitted.height / img_h;

    let img_to_local = Affine2D::from_translate(-img_w / 2.0, -img_h / 2.0)
        .then(Affine2D::from_scale(sx, sy))
        .then(Affine2D::from_translate(
            fitted.center().x,
            fitted.center().y,
        ));
    let full = img_to_local
        .then(placement.scene_transform())
        .then(scene_to_target);

    let paint = PixmapPaint {
        opacity: 1.0,
        blend_mode: BlendMode::SourceOver,
        quality: FilterQuality::Bilinear,
    };
    target.draw_pixmap(0, 0, sticker_pixmap.as_ref(), &paint, full.to_skia(), None);
}

/// Converts a straight-alpha [`RgbaImage`] to a premultiplied [`tiny_skia::Pixmap`].
#[must_use]
pub fn rgba_to_pixmap(image: &RgbaImage) -> Option<Pixmap> {
    let mut pixmap = Pixmap::new(image.width(), image.height())?;
    let src = image.as_raw();
    let dst = pixmap.pixels_mut();
    for (chunk, out) in src.as_chunks::<4>().0.iter().zip(dst.iter_mut()) {
        *out = ColorU8::from_rgba(chunk[0], chunk[1], chunk[2], chunk[3]).premultiply();
    }
    Some(pixmap)
}

/// Converts a premultiplied [`tiny_skia::Pixmap`] back to a straight-alpha [`RgbaImage`].
#[must_use]
pub fn pixmap_to_rgba(pixmap: &Pixmap) -> RgbaImage {
    let mut out = vec![0u8; (pixmap.width() * pixmap.height() * 4) as usize];
    for (&px, chunk) in pixmap
        .pixels()
        .iter()
        .zip(out.as_chunks_mut::<4>().0.iter_mut())
    {
        let straight = demultiply_u8(px);
        chunk[0] = straight.red();
        chunk[1] = straight.green();
        chunk[2] = straight.blue();
        chunk[3] = straight.alpha();
    }
    RgbaImage::from_raw(pixmap.width(), pixmap.height(), out)
        .unwrap_or_else(|| RgbaImage::new(pixmap.width(), pixmap.height()))
}

fn demultiply_u8(px: PremultipliedColorU8) -> ColorU8 {
    px.demultiply()
}

/// Helper to create a solid-color [`Paint`] (used internally by shape masking).
#[allow(dead_code)]
pub(crate) fn solid_white_paint() -> Paint<'static> {
    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    paint.set_color(tiny_skia::Color::WHITE);
    paint
}
