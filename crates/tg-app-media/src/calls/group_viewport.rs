//! Group call video tiles grid layout engine and egui viewport renderer.
//!
//! Port of `Telegram/SourceFiles/calls/group/calls_group_viewport.cpp` and
//! `Telegram/SourceFiles/calls/group/calls_group_viewport_tile.cpp`.

use egui::{Align2, Color32, CornerRadius, FontId, Painter, Pos2, Rect, Sense, Stroke, Ui, Vec2};
use tg_calls::{VideoEndpoint, VideoEndpointType, VideoQuality};
use tg_lang::tr;

use crate::calls::backend::{GroupCallSnapshot, from_calls_peer_id, to_calls_peer_id};
use crate::calls::strings;
use crate::calls::theme::{
    CallPalette, GROUP_CALL_MEMBERS_MARGIN_X, GROUP_CALL_NARROW_MEMBERS_WIDTH,
    GROUP_CALL_NARROW_VIDEO_HEIGHT, GROUP_CALL_VIDEO_LARGE_SKIP, GROUP_CALL_VIDEO_SMALL_SKIP,
    GROUP_CALL_WIDTH, lerp_color, paint_avatar, paint_back_icon, paint_mic_icon,
    paint_screen_share_icon, with_alpha,
};

/// Integer rectangle mirroring `QRect` in `calls_group_viewport.cpp`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TileRect {
    /// Left coordinate in logical pixels.
    pub x: i32,
    /// Top coordinate in logical pixels.
    pub y: i32,
    /// Width in logical pixels.
    pub width: i32,
    /// Height in logical pixels.
    pub height: i32,
}

impl TileRect {
    /// Creates a new [`TileRect`].
    #[must_use]
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Right edge (`x + width`).
    #[must_use]
    pub const fn right(self) -> i32 {
        self.x + self.width
    }

    /// Bottom edge (`y + height`).
    #[must_use]
    pub const fn bottom(self) -> i32 {
        self.y + self.height
    }

    /// Center X coordinate.
    #[must_use]
    pub const fn center_x(self) -> i32 {
        self.x + self.width / 2
    }

    /// Center Y coordinate.
    #[must_use]
    pub const fn center_y(self) -> i32 {
        self.y + self.height / 2
    }

    /// Translates the rectangle by `(dx, dy)`.
    #[must_use]
    pub const fn translated(self, dx: i32, dy: i32) -> Self {
        Self::new(self.x + dx, self.y + dy, self.width, self.height)
    }

    /// Converts to an [`egui::Rect`] offset by `origin`.
    #[must_use]
    pub fn to_egui_rect(self, origin: Pos2) -> Rect {
        Rect::from_min_size(
            Pos2::new(origin.x + self.x as f32, origin.y + self.y as f32),
            Vec2::new(self.width.max(0) as f32, self.height.max(0) as f32),
        )
    }
}

/// Linearly interpolates two [`TileRect`]s (`InterpolateRect`, `calls_group_viewport.cpp:41-53`).
#[must_use]
pub fn interpolate_rect(a: TileRect, b: TileRect, ratio: f64) -> TileRect {
    let r = ratio.clamp(0.0, 1.0);
    let lerp = |u: i32, v: i32| -> i32 { (f64::from(u) + f64::from(v - u) * r).round() as i32 };
    let left = lerp(a.x, b.x);
    let top = lerp(a.y, b.y);
    let right = lerp(a.right(), b.right());
    let bottom = lerp(a.bottom(), b.bottom());
    TileRect::new(left, top, right - left, bottom - top)
}

/// Fallback tile size when video is paused (`kPausedVideoSize = 90` in `calls_group_viewport_tile.cpp:23`).
pub const PAUSED_VIDEO_SIZE: (i32, i32) = (90, 90);

/// Input descriptor for laying out one video tile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoTileInput {
    /// Video endpoint identifier.
    pub endpoint: VideoEndpoint,
    /// Native video frame dimensions `(width, height)`, or `PAUSED_VIDEO_SIZE` for placeholders.
    pub frame_size: (i32, i32),
    /// Whether the tile is paused.
    pub paused: bool,
}

/// Computed layout for a single tile in [`WideTilesLayout`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WideTileGeometry {
    /// Video endpoint.
    pub endpoint: VideoEndpoint,
    /// Native frame dimensions used for aspect-ratio fit.
    pub frame_size: (i32, i32),
    /// Placement in column-major slicing.
    pub columns: TileRect,
    /// Placement in row-major slicing.
    pub rows: TileRect,
}

/// Result of [`compute_wide_layout`] (`Viewport::Layout` in `calls_group_viewport.cpp:527-616`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WideTilesLayout {
    /// Outer viewport dimensions `(width, height)`.
    pub outer: (i32, i32),
    /// Per-tile geometry entries.
    pub list: Vec<WideTileGeometry>,
    /// True if column-major slicing had strictly fewer black letterbox pixels than row-major.
    pub use_columns: bool,
    /// Total letterbox area for column-major layout (useful for tests/diagnostics).
    pub columns_black_area: u64,
    /// Total letterbox area for row-major layout (useful for tests/diagnostics).
    pub rows_black_area: u64,
}

impl WideTilesLayout {
    /// Returns the winning rectangle for tile `index`.
    #[must_use]
    pub fn rect_for(&self, index: usize) -> Option<TileRect> {
        let entry = self.list.get(index)?;
        Some(if self.use_columns {
            entry.columns
        } else {
            entry.rows
        })
    }
}

/// Computes `QSize::scaled(target_w, target_h, Qt::KeepAspectRatio)` for positive dimensions.
#[must_use]
pub fn scale_keep_aspect(src_w: i32, src_h: i32, dst_w: i32, dst_h: i32) -> (i32, i32) {
    if src_w <= 0 || src_h <= 0 || dst_w <= 0 || dst_h <= 0 {
        return (0, 0);
    }
    let use_height = (i64::from(src_w) * i64::from(dst_h)) <= (i64::from(dst_w) * i64::from(src_h));
    if use_height {
        let w = ((f64::from(src_w) * f64::from(dst_h)) / f64::from(src_h)).round() as i32;
        (w.min(dst_w).max(1), dst_h)
    } else {
        let h = ((f64::from(src_h) * f64::from(dst_w)) / f64::from(src_w)).round() as i32;
        (dst_w, h.min(dst_h).max(1))
    }
}

/// Computes the exact wide-mode tile grid layout (`Viewport::countWide`, `calls_group_viewport.cpp:527-616`).
///
/// Compares column-major vs row-major `ceil(sqrt(count))` grids and selects whichever minimizes
/// total unused letterbox area (`columnsBlack < rowsBlack`).
#[must_use]
pub fn compute_wide_layout(
    tiles: &[VideoTileInput],
    outer_width: i32,
    outer_height: i32,
) -> WideTilesLayout {
    let mut list: Vec<WideTileGeometry> = tiles
        .iter()
        .filter(|t| t.frame_size.0 > 0 && t.frame_size.1 > 0)
        .map(|t| WideTileGeometry {
            endpoint: t.endpoint.clone(),
            frame_size: t.frame_size,
            columns: TileRect::default(),
            rows: TileRect::default(),
        })
        .collect();

    if list.is_empty() || outer_width <= 0 || outer_height <= 0 {
        return WideTilesLayout {
            outer: (outer_width, outer_height),
            list,
            use_columns: false,
            columns_black_area: 0,
            rows_black_area: 0,
        };
    }

    if list.len() == 1 {
        let full = TileRect::new(0, 0, outer_width, outer_height);
        list[0].rows = full;
        list[0].columns = full;
        return WideTilesLayout {
            outer: (outer_width, outer_height),
            list,
            use_columns: false,
            columns_black_area: 0,
            rows_black_area: 0,
        };
    }

    let count = list.len();
    let skip = GROUP_CALL_VIDEO_LARGE_SKIP;
    let slices = (count as f64).sqrt().ceil() as usize;

    let mut columns_black: u64 = 0;
    {
        let mut index = 0usize;
        let columns = slices;
        let sizew = f64::from(outer_width + skip) / (columns as f64);
        for column in 0..columns {
            let left = ((column as f64) * sizew).round() as i32;
            let width = (((column as f64) * sizew + sizew - f64::from(skip)).round() as i32) - left;
            let rows = (((count - index) as f64) / ((columns - column) as f64)).round() as usize;
            let sizeh = f64::from(outer_height + skip) / (rows.max(1) as f64);
            for row in 0..rows {
                let top = ((row as f64) * sizeh).round() as i32;
                let height =
                    (((row as f64) * sizeh + sizeh - f64::from(skip)).round() as i32) - top;
                let geom = &mut list[index];
                geom.columns = TileRect::new(left, top, width, height);
                let (sw, sh) =
                    scale_keep_aspect(geom.frame_size.0, geom.frame_size.1, width, height);
                let black = if sw < width {
                    i64::from(width - sw) * i64::from(height)
                } else {
                    i64::from(height - sh) * i64::from(width)
                };
                columns_black = columns_black.saturating_add(black.max(0) as u64);
                index += 1;
            }
        }
    }

    let mut rows_black: u64 = 0;
    {
        let mut index = 0usize;
        let rows = slices;
        let sizeh = f64::from(outer_height + skip) / (rows as f64);
        for row in 0..rows {
            let top = ((row as f64) * sizeh).round() as i32;
            let height = (((row as f64) * sizeh + sizeh - f64::from(skip)).round() as i32) - top;
            let columns = (((count - index) as f64) / ((rows - row) as f64)).round() as usize;
            let sizew = f64::from(outer_width + skip) / (columns.max(1) as f64);
            for column in 0..columns {
                let left = ((column as f64) * sizew).round() as i32;
                let width =
                    (((column as f64) * sizew + sizew - f64::from(skip)).round() as i32) - left;
                let geom = &mut list[index];
                geom.rows = TileRect::new(left, top, width, height);
                let (sw, sh) =
                    scale_keep_aspect(geom.frame_size.0, geom.frame_size.1, width, height);
                let black = if sw < width {
                    i64::from(width - sw) * i64::from(height)
                } else {
                    i64::from(height - sh) * i64::from(width)
                };
                rows_black = rows_black.saturating_add(black.max(0) as u64);
                index += 1;
            }
        }
    }

    WideTilesLayout {
        outer: (outer_width, outer_height),
        list,
        use_columns: columns_black < rows_black,
        columns_black_area: columns_black,
        rows_black_area: rows_black,
    }
}

/// Expands `large` to fill the full viewport and slides neighboring tiles outward for animated
/// spotlight transitions (`Viewport::applyLarge`, `calls_group_viewport.cpp:427-492`).
#[must_use]
pub fn apply_large_layout(
    mut layout: WideTilesLayout,
    large: Option<&VideoEndpoint>,
) -> WideTilesLayout {
    let Some(large_ep) = large else {
        return layout;
    };
    let Some(large_idx) = layout.list.iter().position(|g| &g.endpoint == large_ep) else {
        return layout;
    };

    let (full_w, full_h) = layout.outer;
    let large_rect = if layout.use_columns {
        layout.list[large_idx].columns
    } else {
        layout.list[large_idx].rows
    };
    let large_left = large_rect.x;
    let large_top = large_rect.y;
    let large_right = large_rect.right();
    let large_bottom = large_rect.bottom();

    for (idx, geom) in layout.list.iter_mut().enumerate() {
        if idx == large_idx {
            let full = TileRect::new(0, 0, full_w, full_h);
            if layout.use_columns {
                geom.columns = full;
            } else {
                geom.rows = full;
            }
        } else if layout.use_columns {
            let rect = &mut geom.columns;
            if rect.center_x() < large_left {
                *rect = rect.translated(-large_left, 0);
            } else if rect.center_x() > large_right {
                *rect = rect.translated(full_w - large_right, 0);
            } else if rect.center_y() < large_top {
                *rect = TileRect::new(0, rect.y - large_top, full_w, rect.height);
            } else if rect.center_y() > large_bottom {
                *rect = TileRect::new(0, rect.y + (full_h - large_bottom), full_w, rect.height);
            }
        } else {
            let rect = &mut geom.rows;
            if rect.center_y() < large_top {
                *rect = rect.translated(0, -large_top);
            } else if rect.center_y() > large_bottom {
                *rect = rect.translated(0, full_h - large_bottom);
            } else if rect.center_x() < large_left {
                *rect = TileRect::new(rect.x - large_left, 0, rect.width, full_h);
            } else {
                *rect = TileRect::new(rect.x + (full_w - large_right), 0, rect.width, full_h);
            }
        }
    }
    layout
}

/// Output tile placement for narrow / column modes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionedTile {
    /// Video endpoint.
    pub endpoint: VideoEndpoint,
    /// Placement rectangle.
    pub rect: TileRect,
    /// Requested stream quality (`calls_group_viewport.cpp:857-882`).
    pub quality: VideoQuality,
}

/// Computes narrow-mode tile grid geometry (`Viewport::updateTilesGeometryNarrow`,
/// `calls_group_viewport.cpp:739-806`). Returns `(tiles, total_height)`.
#[must_use]
pub fn compute_narrow_layout(
    tiles: &[VideoTileInput],
    outer_width: i32,
    scroll_top: i32,
) -> (Vec<PositionedTile>, i32) {
    if outer_width <= GROUP_CALL_NARROW_MEMBERS_WIDTH as i32 {
        return compute_column_layout(tiles, None, outer_width, scroll_top);
    }
    let visible: Vec<&VideoTileInput> = tiles
        .iter()
        .filter(|t| t.frame_size.0 > 0 && t.frame_size.1 > 0)
        .collect();
    if visible.is_empty() || outer_width <= 0 {
        return (Vec::new(), 0);
    }
    let y = -scroll_top;
    if visible.len() == 1 {
        let (sw, sh) = visible[0].frame_size;
        let height_min = (outer_width * 9) / 16;
        let height_max = (outer_width * 3) / 4;
        let (_, scaled_h) = scale_keep_aspect(sw, sh, outer_width, height_max);
        let height = scaled_h.max(height_min);
        let rect = TileRect::new(0, y, outer_width, height);
        let quality = compute_tile_video_quality(rect, false, false, false, 1);
        return (
            vec![PositionedTile {
                endpoint: visible[0].endpoint.clone(),
                rect,
                quality,
            }],
            height + GROUP_CALL_VIDEO_SMALL_SKIP,
        );
    }

    let min =
        ((GROUP_CALL_WIDTH as i32) - 2 * GROUP_CALL_MEMBERS_MARGIN_X - GROUP_CALL_VIDEO_SMALL_SKIP)
            / 2;
    let square = (outer_width - GROUP_CALL_VIDEO_SMALL_SKIP) / 2;
    let skip = outer_width - 2 * square;
    let rows = visible.len().div_ceil(2);
    let mut out = Vec::with_capacity(visible.len());

    let mut push_tile = |tile: &VideoTileInput, column: usize, row: usize| {
        let rect = TileRect::new(
            if column == 2 || column == 0 {
                0
            } else {
                outer_width - square
            },
            y + (row as i32) * (min + skip),
            if column == 2 { outer_width } else { square },
            min,
        );
        let quality = compute_tile_video_quality(rect, false, false, false, visible.len());
        out.push(PositionedTile {
            endpoint: tile.endpoint.clone(),
            rect,
            quality,
        });
    };

    if visible.len() == 3 {
        push_tile(visible[0], 2, 0);
        push_tile(visible[1], 0, 1);
        push_tile(visible[2], 1, 1);
    } else {
        let mut row = 0usize;
        let mut column = 0usize;
        for tile in &visible {
            push_tile(tile, column, row);
            if column != 0 {
                row += 1;
                column = if row + 1 == rows && visible.len() % 2 == 1 {
                    2
                } else {
                    0
                };
            } else {
                column = 1;
            }
        }
    }

    (out, (rows as i32) * (min + skip))
}

/// Computes narrow side-column tile list excluding `large` (`Viewport::updateTilesGeometryColumn`,
/// `calls_group_viewport.cpp:808-855`).
#[must_use]
pub fn compute_column_layout(
    tiles: &[VideoTileInput],
    large: Option<&VideoEndpoint>,
    outer_width: i32,
    scroll_top: i32,
) -> (Vec<PositionedTile>, i32) {
    let mut ordered: Vec<&VideoTileInput> = tiles.iter().collect();
    if let Some(large_ep) = large
        && let Some(top_peer) = large_ep.peer
    {
        ordered.sort_by_key(|t| usize::from(t.endpoint.peer != Some(top_peer)));
    }
    let y = -scroll_top;
    let mut top = 0i32;
    let height = GROUP_CALL_NARROW_VIDEO_HEIGHT as i32;
    let mut out = Vec::new();
    for tile in ordered {
        let shown = tile.frame_size.0 > 0
            && tile.frame_size.1 > 0
            && large.is_some_and(|l| l != &tile.endpoint);
        if shown {
            let rect = TileRect::new(0, y + top, outer_width, height);
            let quality = compute_tile_video_quality(rect, false, false, false, 2);
            out.push(PositionedTile {
                endpoint: tile.endpoint.clone(),
                rect,
                quality,
            });
            top += height + GROUP_CALL_VIDEO_SMALL_SKIP;
        }
    }
    (out, top)
}

/// Computes requested [`VideoQuality`] for a tile rectangle (`Viewport::setTileGeometry`,
/// `calls_group_viewport.cpp:857-882`).
#[must_use]
pub const fn compute_tile_video_quality(
    rect: TileRect,
    wide: bool,
    video_stream: bool,
    is_large: bool,
    visible_count: usize,
) -> VideoQuality {
    let min_side = if rect.width < rect.height {
        rect.width
    } else {
        rect.height
    };
    let force_thumbnail = !wide && !video_stream && visible_count > 1;
    let force_full = video_stream || (wide && is_large);
    if force_thumbnail {
        VideoQuality::Thumbnail
    } else if force_full || min_side >= 540 {
        VideoQuality::Full
    } else if min_side >= 240 {
        VideoQuality::Medium
    } else {
        VideoQuality::Thumbnail
    }
}

/// User interaction emitted by [`render_group_viewport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewportAction {
    /// Clicked a tile to focus/unfocus it as the large spotlight view.
    ClickTile(VideoEndpoint),
    /// Clicked the Pin/Unpin overlay button on the spotlight tile.
    TogglePin,
    /// Clicked the Back button to return to the multi-tile grid.
    ClearLarge,
}

/// Collects active video tile inputs from a [`GroupCallSnapshot`].
#[must_use]
pub fn collect_video_tiles(snapshot: &GroupCallSnapshot) -> Vec<VideoTileInput> {
    let mut out = Vec::new();
    for member in &snapshot.members {
        let peer_id = to_calls_peer_id(member.peer.id);
        if let Some(size) = member.screen_frame_size {
            let ep = VideoEndpoint::new(
                VideoEndpointType::Screen,
                peer_id,
                format!("screen-{}", member.peer.id.0),
            );
            out.push(VideoTileInput {
                endpoint: ep,
                frame_size: size,
                paused: member.participant.is_screen_paused(),
            });
        }
        if let Some(size) = member.camera_frame_size {
            let ep = VideoEndpoint::new(
                VideoEndpointType::Camera,
                peer_id,
                format!("cam-{}", member.peer.id.0),
            );
            out.push(VideoTileInput {
                endpoint: ep,
                frame_size: size,
                paused: member.participant.is_camera_paused(),
            });
        }
    }
    out
}

/// Renders the group call video tiles grid inside `rect` using the ported `calls_group_viewport`
/// layout algorithm.
pub fn render_group_viewport(
    ui: &mut Ui,
    rect: Rect,
    snapshot: &GroupCallSnapshot,
    palette: &CallPalette,
    now_ms: u64,
) -> Option<ViewportAction> {
    let tiles = collect_video_tiles(snapshot);
    if tiles.is_empty() {
        return None;
    }

    let outer_w = rect.width().round().max(1.0) as i32;
    let outer_h = rect.height().round().max(1.0) as i32;
    let layout = compute_wide_layout(&tiles, outer_w, outer_h);

    let mut action = None;
    let has_two_or_more = layout.list.len() >= 2;
    let large_ep = snapshot.large_video.as_ref();

    for (idx, geom) in layout.list.iter().enumerate() {
        let is_large = large_ep.is_some_and(|l| l == &geom.endpoint);
        if large_ep.is_some() && !is_large {
            continue;
        }
        let tile_rect = if is_large {
            TileRect::new(0, 0, outer_w, outer_h)
        } else {
            layout.rect_for(idx).unwrap_or_default()
        };
        let screen_rect = tile_rect.to_egui_rect(rect.min);
        let Some(member) = snapshot.members.iter().find(|m| {
            geom.endpoint
                .peer
                .is_some_and(|p| from_calls_peer_id(p) == m.peer.id)
        }) else {
            continue;
        };
        let tile_input = &tiles[idx];

        let resp = ui.allocate_rect(screen_rect, Sense::click());
        let painter = ui.painter_at(screen_rect);

        paint_video_tile(&painter, screen_rect, member, tile_input, palette, now_ms);

        // Top overlay controls (Back + Pin/Unpin) when viewing a spotlighted tile among >= 2 tiles.
        if is_large && has_two_or_more {
            let back_rect = Rect::from_min_size(
                Pos2::new(screen_rect.left() + 14.0, screen_rect.top() + 14.0),
                Vec2::new(72.0, 26.0),
            );
            let pin_label = if snapshot.large_video_pinned {
                strings::UNPIN_LABEL
            } else {
                strings::PIN_LABEL
            };
            let pin_rect = Rect::from_min_size(
                Pos2::new(screen_rect.right() - 78.0, screen_rect.top() + 14.0),
                Vec2::new(64.0, 26.0),
            );

            paint_back_tile_pill(&painter, back_rect, &tr::lng_create_group_back(), palette);
            paint_tile_pill(&painter, pin_rect, pin_label, palette);

            if resp.clicked()
                && let Some(pos) = resp.interact_pointer_pos()
            {
                if back_rect.contains(pos) {
                    action = Some(ViewportAction::ClearLarge);
                } else if pin_rect.contains(pos) {
                    action = Some(ViewportAction::TogglePin);
                } else {
                    action = Some(ViewportAction::ClickTile(geom.endpoint.clone()));
                }
            }
        } else if resp.clicked() {
            action = Some(ViewportAction::ClickTile(geom.endpoint.clone()));
        }
    }

    action
}

fn paint_back_tile_pill(painter: &Painter, rect: Rect, text: &str, palette: &CallPalette) {
    painter.rect_filled(
        rect,
        CornerRadius::same(13),
        Color32::from_rgba_unmultiplied(18, 22, 27, 175),
    );
    paint_back_icon(
        painter,
        Pos2::new(rect.left() + 14.0, rect.center().y),
        13.0,
        palette.group_video_text_fg,
    );
    painter.text(
        Pos2::new(rect.left() + 25.0, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        FontId::proportional(12.0),
        palette.group_video_text_fg,
    );
}

fn paint_tile_pill(painter: &Painter, rect: Rect, text: &str, palette: &CallPalette) {
    painter.rect_filled(
        rect,
        CornerRadius::same(13),
        Color32::from_rgba_unmultiplied(18, 22, 27, 175),
    );
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        FontId::proportional(12.0),
        palette.group_video_text_fg,
    );
}

fn paint_video_tile(
    painter: &Painter,
    rect: Rect,
    member: &crate::calls::backend::GroupMemberView,
    tile: &VideoTileInput,
    palette: &CallPalette,
    now_ms: u64,
) {
    let radius = CornerRadius::same(10);
    let is_screen = tile.endpoint.kind == VideoEndpointType::Screen;

    // Rich synthetic camera/screen background so demo tiles are immediately legible without a webcam.
    let base_a = if is_screen {
        Color32::from_rgb(0x16, 0x24, 0x36)
    } else {
        lerp_color(
            member.peer.avatar_color,
            Color32::from_rgb(0x15, 0x1B, 0x22),
            0.62,
        )
    };
    let base_b = if is_screen {
        Color32::from_rgb(0x0F, 0x18, 0x24)
    } else {
        Color32::from_rgb(0x1A, 0x21, 0x29)
    };
    painter.rect_filled(rect, radius, base_b);
    painter.rect_filled(rect.shrink(1.0), radius, base_a);

    if tile.paused {
        painter.text(
            Pos2::new(rect.center().x, rect.center().y - 6.0),
            Align2::CENTER_CENTER,
            tr::lng_group_call_video_paused(),
            FontId::proportional(14.0),
            palette.group_video_text_fg,
        );
    } else if is_screen {
        // Synthetic code/slides presentation preview for screen-share tiles.
        let card = rect.shrink2(Vec2::new(rect.width() * 0.08, rect.height() * 0.14));
        painter.rect_filled(
            card,
            CornerRadius::same(8),
            Color32::from_rgba_unmultiplied(12, 18, 26, 210),
        );
        painter.rect_stroke(
            card,
            CornerRadius::same(8),
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(110, 180, 255, 65)),
            egui::StrokeKind::Inside,
        );
        // Header dots + synthetic slide/architecture bars.
        let header_y = card.top() + 14.0;
        for (i, c) in [
            Color32::from_rgb(0xED, 0x6A, 0x5E),
            Color32::from_rgb(0xF4, 0xBF, 0x4F),
            Color32::from_rgb(0x61, 0xC5, 0x54),
        ]
        .iter()
        .enumerate()
        {
            painter.circle_filled(
                Pos2::new(card.left() + 14.0 + (i as f32) * 12.0, header_y),
                3.5,
                *c,
            );
        }
        painter.text(
            Pos2::new(card.left() + 56.0, header_y),
            Align2::LEFT_CENTER,
            strings::SCREEN_TILE_DEMO_HEADER,
            FontId::proportional(11.0),
            Color32::from_rgb(0xA6, 0xC4, 0xE0),
        );
        let line_top = card.top() + 34.0;
        let line_widths = [0.72_f32, 0.56, 0.84, 0.45, 0.68];
        for (i, frac) in line_widths.iter().enumerate() {
            let y = line_top + (i as f32) * 16.0;
            if y + 8.0 > card.bottom() - 12.0 {
                break;
            }
            let bar = Rect::from_min_size(
                Pos2::new(card.left() + 16.0, y),
                Vec2::new((card.width() - 32.0) * *frac, 8.0),
            );
            let accent = if i == 0 {
                palette.group_active_fg
            } else {
                Color32::from_rgba_unmultiplied(180, 210, 240, 55)
            };
            painter.rect_filled(bar, CornerRadius::same(4), accent);
        }
    } else {
        // Synthetic camera feed glow + centered avatar with subtle speaking ring.
        let glow_r = rect.height().min(rect.width()) * 0.38;
        let pulse = 1.0 + 0.04 * ((now_ms as f32 * 0.004).sin());
        painter.circle_filled(
            rect.center(),
            glow_r * pulse,
            with_alpha(member.peer.avatar_color, 0.24),
        );
        let avatar_r = (rect.height().min(rect.width()) * 0.20).clamp(24.0, 44.0);
        paint_avatar(
            painter,
            Pos2::new(rect.center().x, rect.center().y - 8.0),
            avatar_r,
            member.peer.avatar_color,
            &member.peer.initials,
            avatar_r * 0.72,
        );
    }

    // Bottom gradient shadow bar (`groupCallVideoTile.shadowHeight = 40`).
    let shadow_h = 36.0_f32.min(rect.height() * 0.35);
    let shadow_rect =
        Rect::from_min_max(Pos2::new(rect.left(), rect.bottom() - shadow_h), rect.max);
    painter.rect_filled(
        shadow_rect,
        CornerRadius {
            nw: 0,
            ne: 0,
            sw: 10,
            se: 10,
        },
        Color32::from_rgba_unmultiplied(10, 14, 18, 175),
    );

    // Bottom-left icon + participant name.
    let label_y = shadow_rect.center().y;
    let icon_center = Pos2::new(rect.left() + 18.0, label_y);
    if is_screen {
        paint_screen_share_icon(painter, icon_center, 15.0, palette.group_video_text_fg);
    } else {
        let mic_color = if member.participant.speaking {
            palette.group_member_active
        } else {
            palette.group_video_text_fg
        };
        let slash = if member.participant.muted { 1.0 } else { 0.0 };
        paint_mic_icon(
            painter,
            icon_center,
            15.0,
            mic_color,
            slash,
            palette.group_member_muted_icon,
        );
    }
    let name_text = if is_screen {
        format!("{} {}", member.peer.name, strings::SCREEN_SUFFIX)
    } else {
        member.peer.name.clone()
    };
    painter.text(
        Pos2::new(rect.left() + 32.0, label_y),
        Align2::LEFT_CENTER,
        name_text,
        FontId::proportional(13.0),
        palette.group_video_text_fg,
    );

    // Speaking green outline (`groupCallOutline = 2px`).
    if member.participant.speaking || member.audio_level >= tg_calls::group::SPEAK_LEVEL_THRESHOLD {
        painter.rect_stroke(
            rect.shrink(1.0),
            radius,
            Stroke::new(2.5, palette.group_member_active),
            egui::StrokeKind::Inside,
        );
    } else {
        painter.rect_stroke(
            rect,
            radius,
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 22)),
            egui::StrokeKind::Inside,
        );
    }
}
