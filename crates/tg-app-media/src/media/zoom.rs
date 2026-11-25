//! Media viewer zoom levels, fit-to-screen computation, anchored wheel zoom, and pan clamping.
//!
//! Port origin: `Telegram/SourceFiles/media/view/media_view_overlay_widget.cpp`
//! (`kMaxZoomLevel`, `kZoomToScreenLevel`, `contentSizeChanged`, `resizeContentByScreenSize`,
//! `zoomIn`, `zoomOut`, `zoomReset`, `zoomUpdate`, `setZoomLevel`, `snapXY`).

use egui::{Pos2, Rect, Vec2};

/// Maximum discrete zoom level (`kMaxZoomLevel = 7`, i.e. 8× magnification or 1/8× reduction).
pub const MAX_ZOOM_LEVEL: i32 = 7;

/// Sentinel zoom level representing fit-to-screen / fit-to-default (`kZoomToScreenLevel = 1024`).
pub const ZOOM_TO_SCREEN_LEVEL: i32 = 1024;

/// Pinch gesture zoom step (`kPinchZoomStep = 0.25`).
pub const PINCH_ZOOM_STEP: f64 = 0.25;

/// Computes tdesktop's signed zoom value to fit `(content_w, content_h)` inside `(outer_w, outer_h)`.
///
/// Mirrors `countZoomFor` in `media_view_overlay_widget.cpp:2866-2877`:
/// - When scale `r >= 1.0`, returns `r - 1.0` (so `w * (z + 1) == w * r`).
/// - When scale `r < 1.0`, returns `1.0 - (1.0 / r)` (so `w / (-z + 1) == w * r`).
#[must_use]
pub fn count_zoom_for(content_w: f64, content_h: f64, outer_w: f64, outer_h: f64) -> f64 {
    if content_w <= 0.0 || content_h <= 0.0 || outer_w <= 0.0 || outer_h <= 0.0 {
        return 0.0;
    }
    let mut ratio = outer_w / content_w;
    if content_h * ratio > outer_h {
        ratio = outer_h / content_h;
    }
    if ratio <= 0.0 {
        return 0.0;
    }
    if ratio >= 1.0 {
        ratio - 1.0
    } else {
        1.0 - (1.0 / ratio)
    }
}

/// Converts a tdesktop signed zoom value `z` into a linear scale multiplier.
#[must_use]
pub fn zoom_value_to_scale(z: f64) -> f64 {
    if z >= 0.0 { z + 1.0 } else { 1.0 / (-z + 1.0) }
}

/// Normalizes an arbitrary clockwise rotation in degrees to `{0, 90, 180, 270}`.
#[must_use]
pub const fn normalize_rotation(degrees: i32) -> i32 {
    let r = degrees % 360;
    let pos = if r < 0 { r + 360 } else { r };
    match pos {
        90 | 180 | 270 => pos,
        _ => 0,
    }
}

/// Swaps width and height when rotation is 90° or 270° (`FlipSizeByRotation`).
#[must_use]
pub const fn flip_size_by_rotation(width: f64, height: f64, rotation: i32) -> (f64, f64) {
    if (normalize_rotation(rotation) / 90) % 2 == 1 {
        (height, width)
    } else {
        (width, height)
    }
}

/// Complete viewport, zoom, pan, rotation, and flip state for the media viewer.
#[derive(Debug, Clone, PartialEq)]
pub struct ZoomState {
    /// Original unrotated content width in logical pixels.
    pub base_width: f64,
    /// Original unrotated content height in logical pixels.
    pub base_height: f64,
    /// Effective unzoomed width after rotation (`_width`).
    pub width: f64,
    /// Effective unzoomed height after rotation (`_height`).
    pub height: f64,
    /// Viewport width.
    pub viewport_w: f64,
    /// Viewport height.
    pub viewport_h: f64,
    /// Top inset below header/notch (`_skipTop`).
    pub skip_top: f64,
    /// Available vertical height excluding top and bottom overlays (`_availableHeight`).
    pub available_height: f64,
    /// Minimum usable top coordinate (`_minUsedTop`).
    pub min_used_top: f64,
    /// Maximum usable height (`_maxUsedHeight`).
    pub max_used_height: f64,
    /// Displayed content X coordinate in viewport space (`_x`).
    pub x: f64,
    /// Displayed content Y coordinate in viewport space (`_y`).
    pub y: f64,
    /// Displayed content width (`_w`).
    pub w: f64,
    /// Displayed content height (`_h`).
    pub h: f64,
    /// Discrete zoom level: `< 0` zoomed out, `0` = 100%, `> 0` zoomed in, `ZOOM_TO_SCREEN_LEVEL` = fit.
    pub zoom: i32,
    /// Signed zoom value fitting content inside default padded area (`_zoomToDefault`).
    pub zoom_to_default: f64,
    /// Signed zoom value fitting content inside full screen (`_zoomToScreen`).
    pub zoom_to_screen: f64,
    /// Whether fullscreen video mode is active (`_fullScreenVideo`).
    pub full_screen_video: bool,
    /// Saved zoom level before entering fullscreen video (`_fullScreenZoomCache`).
    pub full_screen_zoom_cache: i32,
    /// Clockwise rotation in degrees (`0`, `90`, `180`, `270`).
    pub rotation: i32,
    /// Horizontal mirror flip (`H` shortcut).
    pub flip_h: bool,
    /// Vertical mirror flip (`V` shortcut).
    pub flip_v: bool,
}

impl Default for ZoomState {
    fn default() -> Self {
        Self {
            base_width: 0.0,
            base_height: 0.0,
            width: 0.0,
            height: 0.0,
            viewport_w: 800.0,
            viewport_h: 600.0,
            skip_top: 11.0,
            available_height: 578.0,
            min_used_top: 0.0,
            max_used_height: 600.0,
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
            zoom: 0,
            zoom_to_default: 0.0,
            zoom_to_screen: 0.0,
            full_screen_video: false,
            full_screen_zoom_cache: 0,
            rotation: 0,
            flip_h: false,
            flip_v: false,
        }
    }
}

impl ZoomState {
    /// Initializes zoom state for a new media item of size `(content_w, content_h)` inside a viewport.
    #[must_use]
    pub fn new(content_w: f64, content_h: f64, viewport_w: f64, viewport_h: f64) -> Self {
        let mut state = Self::default();
        state.set_content(content_w, content_h, 0);
        state.resize_viewport(viewport_w, viewport_h, 0.0, 0.0);
        state
    }

    /// Sets new base content dimensions and rotation, resetting flip and fitting to viewport.
    pub fn set_content(&mut self, base_w: f64, base_h: f64, rotation_degrees: i32) {
        self.base_width = base_w.max(1.0);
        self.base_height = base_h.max(1.0);
        self.rotation = normalize_rotation(rotation_degrees);
        self.flip_h = false;
        self.flip_v = false;
        let (eff_w, eff_h) =
            flip_size_by_rotation(self.base_width, self.base_height, self.rotation);
        self.width = eff_w;
        self.height = eff_h;
        self.full_screen_video = false;
        self.resize_content_by_screen_size();
    }

    /// Updates viewport dimensions and overlay insets (`bottom_controls_skip`), recomputing fit zoom.
    pub fn resize_viewport(
        &mut self,
        viewport_w: f64,
        viewport_h: f64,
        min_used_top: f64,
        bottom_controls_skip: f64,
    ) {
        self.viewport_w = viewport_w.max(1.0);
        self.viewport_h = viewport_h.max(1.0);
        self.min_used_top = min_used_top.max(0.0);
        self.max_used_height = (self.viewport_h - self.min_used_top).max(1.0);
        self.recount_skip_top(bottom_controls_skip);
        self.resize_content_by_screen_size();
    }

    /// Recomputes `skip_top` and `available_height` (mirrors `OverlayWidget::recountSkipTop`).
    pub fn recount_skip_top(&mut self, bottom_controls_skip: f64) {
        let skip_bottom = bottom_controls_skip.clamp(0.0, self.viewport_h * 0.5);
        let caption_margin = 11.0;
        let inner_gap = (self.viewport_h - self.height - skip_bottom).max(caption_margin);
        self.skip_top = self.min_used_top + inner_gap.min(skip_bottom);
        self.available_height = (self.viewport_h - skip_bottom - self.skip_top).max(1.0);
        if self.full_screen_video && skip_bottom > 0.0 && self.width > 0.0 {
            let h = self.viewport_w * self.height / self.width;
            let top_all_fit = self.max_used_height - skip_bottom - h;
            if self.skip_top > top_all_fit {
                self.skip_top = top_all_fit.max(0.0);
            }
        }
    }

    /// Recomputes `_zoomToDefault`, `_zoomToScreen`, and initial centered fit rect.
    ///
    /// Exact port of `OverlayWidget::resizeContentByScreenSize` (`media_view_overlay_widget.cpp:2843-2906`).
    pub fn resize_content_by_screen_size(&mut self) {
        let avail_w = self.viewport_w;
        if self.width > 0.0 && self.height > 0.0 {
            self.zoom_to_default =
                count_zoom_for(self.width, self.height, avail_w, self.available_height);
            self.zoom_to_screen = count_zoom_for(
                self.width,
                self.height,
                self.viewport_w,
                self.max_used_height,
            );
        } else {
            self.zoom_to_default = 0.0;
            self.zoom_to_screen = 0.0;
        }

        let use_w = if self.full_screen_video {
            self.viewport_w
        } else {
            avail_w
        };
        let use_h = if self.full_screen_video {
            self.max_used_height
        } else {
            self.available_height
        };

        if self.width > use_w || self.height > use_h || self.full_screen_video {
            let use_z = self.full_zoom_value();
            self.zoom = ZOOM_TO_SCREEN_LEVEL;
            let scale = zoom_value_to_scale(use_z);
            self.w = (self.width * scale).round().max(1.0);
            self.h = (self.height * scale).round().max(1.0);
        } else {
            self.zoom = 0;
            self.w = self.width;
            self.h = self.height;
        }

        self.x = ((self.viewport_w - self.w) / 2.0).round();
        self.y = if self.full_screen_video {
            (self.min_used_top + (self.max_used_height - self.h) / 2.0).round()
        } else {
            (self.skip_top + (use_h - self.h) / 2.0).round()
        };
    }

    /// Returns the fit zoom value for the current mode (`_fullScreenVideo ? _zoomToScreen : _zoomToDefault`).
    #[must_use]
    pub const fn full_zoom_value(&self) -> f64 {
        if self.full_screen_video {
            self.zoom_to_screen
        } else {
            self.zoom_to_default
        }
    }

    /// Returns the effective signed zoom value currently applied.
    #[must_use]
    pub fn current_zoom_value(&self) -> f64 {
        if self.zoom == ZOOM_TO_SCREEN_LEVEL {
            self.full_zoom_value()
        } else {
            f64::from(self.zoom)
        }
    }

    /// Returns the current linear scale factor (e.g. `1.0` for 100%, `2.0` for 200%, `0.5` for 50%).
    #[must_use]
    pub fn scale_factor(&self) -> f64 {
        zoom_value_to_scale(self.current_zoom_value())
    }

    /// Returns the current scale percentage rounded to the nearest integer (e.g. `100`).
    #[must_use]
    pub fn zoom_percent(&self) -> u32 {
        (self.scale_factor() * 100.0).round().max(1.0) as u32
    }

    /// Returns the displayed content rectangle in viewport coordinates.
    #[must_use]
    pub fn content_rect(&self) -> Rect {
        Rect::from_min_size(
            Pos2::new(self.x as f32, self.y as f32),
            Vec2::new(self.w as f32, self.h as f32),
        )
    }

    /// Returns `true` when the displayed media exceeds the viewport and can be panned by dragging.
    #[must_use]
    pub fn can_drag(&self) -> bool {
        self.w > self.viewport_w + 0.5 || self.h > self.available_height + 0.5
    }

    /// Steps zoom in by one level around an optional anchor point (`OverlayWidget::zoomIn`).
    pub fn zoom_in(&mut self, anchor: Option<(f64, f64)>) {
        let mut new_zoom = self.zoom;
        let full = self.full_zoom_value();
        if new_zoom == ZOOM_TO_SCREEN_LEVEL {
            if (full.ceil() as i32) <= MAX_ZOOM_LEVEL {
                new_zoom = full.ceil() as i32;
            }
        } else if f64::from(new_zoom) < full
            && (f64::from(new_zoom + 1) > full
                || (full > f64::from(MAX_ZOOM_LEVEL) && new_zoom == MAX_ZOOM_LEVEL))
        {
            new_zoom = ZOOM_TO_SCREEN_LEVEL;
        } else if new_zoom < MAX_ZOOM_LEVEL {
            new_zoom += 1;
        }
        self.zoom_update(new_zoom, anchor);
    }

    /// Steps zoom out by one level around an optional anchor point (`OverlayWidget::zoomOut`).
    pub fn zoom_out(&mut self, anchor: Option<(f64, f64)>) {
        let mut new_zoom = self.zoom;
        let full = self.full_zoom_value();
        if new_zoom == ZOOM_TO_SCREEN_LEVEL {
            if (full.floor() as i32) >= -MAX_ZOOM_LEVEL {
                new_zoom = full.floor() as i32;
            }
        } else if f64::from(new_zoom) > full
            && (f64::from(new_zoom - 1) < full
                || (full < f64::from(-MAX_ZOOM_LEVEL) && new_zoom == -MAX_ZOOM_LEVEL))
        {
            new_zoom = ZOOM_TO_SCREEN_LEVEL;
        } else if new_zoom > -MAX_ZOOM_LEVEL {
            new_zoom -= 1;
        }
        self.zoom_update(new_zoom, anchor);
    }

    /// Toggles between 100% (`zoom == 0`) and fit-to-screen (`OverlayWidget::zoomReset`).
    pub fn zoom_reset(&mut self) {
        if self.full_screen_video {
            return;
        }
        let full = self.full_zoom_value();
        let new_zoom = if self.zoom == 0 {
            if (full.floor() as i32) == (full.ceil() as i32)
                && (full.round() as i32) >= -MAX_ZOOM_LEVEL
                && (full.round() as i32) <= MAX_ZOOM_LEVEL
            {
                full.round() as i32
            } else {
                ZOOM_TO_SCREEN_LEVEL
            }
        } else {
            0
        };

        let z = self.current_zoom_value();
        let mut x = -self.width / 2.0;
        let mut y = self.skip_top - (self.height / 2.0);
        if z >= 0.0 {
            x = (x * (z + 1.0)).round();
            y = (y * (z + 1.0)).round();
        } else {
            x = (x / (-z + 1.0)).round();
            y = (y / (-z + 1.0)).round();
        }
        self.x = x + (self.viewport_w / 2.0);
        self.y = y + (self.available_height / 2.0);
        self.zoom_update(new_zoom, None);
    }

    /// Clamps tiny negative zoom levels before applying `set_zoom_level` (`OverlayWidget::zoomUpdate`).
    pub fn zoom_update(&mut self, mut new_zoom: i32, anchor: Option<(f64, f64)>) {
        if new_zoom != ZOOM_TO_SCREEN_LEVEL {
            while new_zoom < 0
                && (f64::from(-new_zoom + 1) > self.width || f64::from(-new_zoom + 1) > self.height)
            {
                new_zoom += 1;
            }
        }
        self.set_zoom_level(new_zoom, false, anchor);
    }

    /// Applies `new_zoom`, preserving the pixel under `anchor` (or viewport center) and snapping bounds.
    ///
    /// Exact port of `OverlayWidget::setZoomLevel` (`media_view_overlay_widget.cpp:7764-7818`).
    pub fn set_zoom_level(&mut self, new_zoom: i32, force: bool, anchor: Option<(f64, f64)>) {
        if (!force && self.zoom == new_zoom)
            || (self.full_screen_video && new_zoom != ZOOM_TO_SCREEN_LEVEL)
        {
            return;
        }

        let full = self.full_zoom_value();
        let old_z = if self.zoom == ZOOM_TO_SCREEN_LEVEL {
            full
        } else {
            f64::from(self.zoom)
        };
        let (anchor_x, anchor_y) =
            anchor.unwrap_or((self.viewport_w / 2.0, self.available_height / 2.0));

        let (nx, ny) = if old_z >= 0.0 {
            (
                (self.x - anchor_x) / (old_z + 1.0),
                (self.y - anchor_y) / (old_z + 1.0),
            )
        } else {
            (
                (self.x - anchor_x) * (-old_z + 1.0),
                (self.y - anchor_y) * (-old_z + 1.0),
            )
        };

        self.zoom = new_zoom;
        let new_z = if self.zoom == ZOOM_TO_SCREEN_LEVEL {
            full
        } else {
            f64::from(self.zoom)
        };
        if new_z > 0.0 {
            self.w = (self.width * (new_z + 1.0)).round().max(1.0);
            self.h = (self.height * (new_z + 1.0)).round().max(1.0);
            self.x = (nx * (new_z + 1.0) + anchor_x).round();
            self.y = (ny * (new_z + 1.0) + anchor_y).round();
        } else {
            self.w = (self.width / (-new_z + 1.0)).round().max(1.0);
            self.h = (self.height / (-new_z + 1.0)).round().max(1.0);
            self.x = (nx / (-new_z + 1.0) + anchor_x).round();
            self.y = (ny / (-new_z + 1.0) + anchor_y).round();
        }
        self.snap_xy();
    }

    /// Pans the zoomed image by `(dx, dy)` and clamps within bounds.
    pub fn drag_by(&mut self, dx: f64, dy: f64) {
        self.x += dx;
        self.y += dy;
        self.snap_xy();
    }

    /// Clamps `(x, y)` so content never drifts off-screen and centers when smaller than viewport.
    ///
    /// Exact port of `OverlayWidget::snapXY` (`media_view_overlay_widget.cpp:8276-8290`).
    pub fn snap_xy(&mut self) {
        let center_x = (self.viewport_w - self.w) / 2.0;
        let xmin = (self.viewport_w - self.w).min(center_x);
        let xmax = 0.0_f64.max(center_x);

        let center_y = if self.full_screen_video {
            self.min_used_top + (self.max_used_height - self.h) / 2.0
        } else {
            self.skip_top + (self.available_height - self.h) / 2.0
        };
        let ymin = (self.viewport_h - self.h).min(center_y);
        let ymax = self.min_used_top.max(center_y);

        self.x = self.x.clamp(xmin, xmax);
        self.y = self.y.clamp(ymin, ymax);
    }

    /// Rotates content 90° clockwise, transposing effective dimensions and re-fitting.
    pub fn rotate_cw(&mut self) {
        self.rotation = normalize_rotation(self.rotation + 90);
        let (eff_w, eff_h) =
            flip_size_by_rotation(self.base_width, self.base_height, self.rotation);
        self.width = eff_w;
        self.height = eff_h;
        self.resize_content_by_screen_size();
    }

    /// Rotates content 90° counter-clockwise, transposing effective dimensions and re-fitting.
    pub fn rotate_ccw(&mut self) {
        self.rotation = normalize_rotation(self.rotation - 90);
        let (eff_w, eff_h) =
            flip_size_by_rotation(self.base_width, self.base_height, self.rotation);
        self.width = eff_w;
        self.height = eff_h;
        self.resize_content_by_screen_size();
    }

    /// Toggles fullscreen video zoom mode (mirrors `OverlayWidget::playbackToggleFullScreen`).
    pub fn toggle_fullscreen_video(&mut self, bottom_controls_skip: f64) {
        self.full_screen_video = !self.full_screen_video;
        if self.full_screen_video {
            self.full_screen_zoom_cache = self.zoom;
        }
        self.recount_skip_top(bottom_controls_skip);
        let target = if self.full_screen_video {
            ZOOM_TO_SCREEN_LEVEL
        } else {
            self.full_screen_zoom_cache
        };
        self.set_zoom_level(target, true, None);
    }
}
