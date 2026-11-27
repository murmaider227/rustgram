//! Pure layout geometry, slider math, rotated interactive-area projection,
//! timestamp formatting, and dialogs-strip interpolation ported from tdesktop.
//!
//! Port origin:
//! - `Telegram/SourceFiles/media/stories/media_stories_controller.cpp` (`initLayout`, `Rotated`, `closeByClickAt`)
//! - `Telegram/SourceFiles/media/stories/media_stories_slider.cpp` (`Slider::layout`, `Slider::paint`)
//! - `Telegram/SourceFiles/media/stories/media_stories_header.cpp` (`ComposeTimestamp`)
//! - `Telegram/SourceFiles/media/stories/media_stories_reactions.cpp` (`ChooseWeatherFg`)
//! - `Telegram/SourceFiles/dialogs/ui/dialogs_stories_list.cpp` (`computeLayout`, `Decelerate`, `EaseOutQuint`)
//! - `Telegram/SourceFiles/media/view/media_view.style` & `dialogs/dialogs.style`

use std::f32::consts::PI;

use egui::{Color32, Pos2, Rect, Vec2};
use tg_data::TimeId;
use tg_stories::{
    COLLAPSE_AFTER_RATIO, DialogStoriesBarState, EXPAND_AFTER_RATIO, FRICTION_RATIO,
    SMALL_THUMBS_SHOWN, SliderState, StoryArea, rotate_point,
};

// ---------------------------------------------------------------------------
// Viewer constants matching `media/view/media_view.style` & `media_stories_controller.cpp`
// ---------------------------------------------------------------------------

/// Maximum portrait story content size (`st::storiesMaxSize = 540x960`).
pub const STORIES_MAX_WIDTH: f32 = 540.0;
/// Maximum portrait story content height (`st::storiesMaxSize = 540x960`).
pub const STORIES_MAX_HEIGHT: f32 = 960.0;
/// Story card corner rounding radius (`st::storiesRadius = 8px`).
pub const STORIES_RADIUS: f32 = 8.0;
/// Width/height of side prev/next arrow controls (`st::storiesControlSize = 64px`).
pub const STORIES_CONTROL_SIZE: f32 = 64.0;
/// Horizontal side margin for content frame (`st::storiesSideSkip = 145px`).
pub const STORIES_SIDE_SKIP: f32 = 145.0;
/// Minimum sibling preview width (`st::storiesSiblingWidthMin = 200px`).
pub const STORIES_SIBLING_WIDTH_MIN: f32 = 200.0;
/// Maximum sibling name font size (`st::storiesMaxNameFontSize = 17px`).
pub const STORIES_MAX_NAME_FONT_SIZE: f32 = 17.0;
/// Minimum controls footer width (`st::storiesControlsMinWidth = 280px`).
pub const STORIES_CONTROLS_MIN_WIDTH: f32 = 280.0;
/// Extra horizontal extension of footer controls beyond story frame (`st::storiesControlsExtend = 4px`).
pub const STORIES_CONTROLS_EXTEND: f32 = 4.0;
/// Slider segment bar thickness (`st::storiesSliderWidth = 2px`).
pub const STORIES_SLIDER_HEIGHT: f32 = 2.0;
/// Gap between adjacent slider segments (`st::storiesSliderSkip = 4px`).
pub const STORIES_SLIDER_SKIP: f32 = 4.0;
/// Horizontal margin inside slider rect (`st::storiesSliderMargin.left/right = 8px`).
pub const STORIES_SLIDER_MARGIN_X: f32 = 8.0;
/// Top margin of slider rect (`st::storiesSliderMargin.top = 7px`).
pub const STORIES_SLIDER_MARGIN_TOP: f32 = 7.0;
/// Bottom margin of slider rect (`st::storiesSliderMargin.bottom = 6px`).
pub const STORIES_SLIDER_MARGIN_BOTTOM: f32 = 6.0;
/// Extra spacing between outside header and content (`st::storiesSliderOutsideSkip = 4px`).
pub const STORIES_SLIDER_OUTSIDE_SKIP: f32 = 4.0;
/// Header avatar diameter (`st::storiesHeaderPhoto.photoSize = 28px`).
pub const STORIES_HEADER_PHOTO_SIZE: f32 = 28.0;
/// Header vertical height (`4 + 28 + 8 = 40px`).
pub const STORIES_HEADER_HEIGHT: f32 = 40.0;
/// Bottom reply/views bar height (`18 + 42 + 12 = 72px`).
pub const STORIES_FIELD_MIN_HEIGHT: f32 = 72.0;
/// Bottom outer margin (`st::storiesFieldMargin.bottom = 12px`).
pub const STORIES_FIELD_BOTTOM_MARGIN: f32 = 12.0;
/// Default sibling scale factor (`kSiblingMultiplierDefault = 0.448`).
pub const SIBLING_MULTIPLIER_DEFAULT: f32 = 0.448;
/// Maximum sibling scale factor (`kSiblingMultiplierMax = 0.72`).
pub const SIBLING_MULTIPLIER_MAX: f32 = 0.72;
/// Fraction of sibling allowed offscreen (`kSiblingOutsidePart = 0.24`).
pub const SIBLING_OUTSIDE_PART: f32 = 0.24;
/// Sibling avatar size ratio relative to sibling width (`kSiblingUserpicSize = 0.3`).
pub const SIBLING_USERPIC_RATIO: f32 = 0.3;
/// Dimming opacity when reply or full caption is open (`kFullContentFade = 0.6`).
pub const FULL_CONTENT_FADE: f32 = 0.6;
/// Sibling card default dimming opacity (`kSiblingFade = 0.5`).
pub const SIBLING_FADE: f32 = 0.5;
/// Sibling card hover dimming opacity (`kSiblingFadeOver = 0.4`).
pub const SIBLING_FADE_OVER: f32 = 0.4;
/// Sibling card hover scale bump (`kSiblingScaleOver = 0.05`).
pub const SIBLING_SCALE_OVER: f32 = 0.05;

/// Header placement mode (`Media::Stories::HeaderLayout`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum HeaderLayout {
    /// Slider and header sit inside the top of the portrait story frame.
    Normal,
    /// Tall window: header and slider sit cleanly above the story frame.
    Outside,
}

/// Computed sibling preview card geometry (`Media::Stories::SiblingLayout`).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct SiblingCardLayout {
    /// Outer card bounding rectangle.
    pub geometry: Rect,
    /// Centered peer avatar rectangle.
    pub userpic: Rect,
    /// Bounding rectangle for peer short name.
    pub name_rect: Rect,
    /// Scaled name font size in logical pixels.
    pub name_font_size: f32,
}

/// Complete story viewer layout for a viewport rectangle (`Media::Stories::Layout`).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ViewerLayout {
    /// Viewport bounds.
    pub viewport: Rect,
    /// Header placement mode (`Normal` vs `Outside`).
    pub header_layout: HeaderLayout,
    /// Primary 9:16 portrait story content frame.
    pub content: Rect,
    /// Top segmented progress slider bar bounds (with inner margins already applied).
    pub slider: Rect,
    /// Header strip bounds (avatar, peer name, timestamp, play/pause, volume, menu).
    pub header: Rect,
    /// Bottom reply/views bar rectangle.
    pub controls: Rect,
    /// Left sibling preview card geometry.
    pub sibling_left: SiblingCardLayout,
    /// Right sibling preview card geometry.
    pub sibling_right: SiblingCardLayout,
    /// Left navigation arrow hit area.
    pub prev_arrow: Rect,
    /// Right navigation arrow hit area.
    pub next_arrow: Rect,
    /// Top-right close (`X`) button rectangle.
    pub close_button: Rect,
}

impl ViewerLayout {
    /// Computes the exact tdesktop story viewer layout inside `viewport`
    /// (`Controller::initLayout` in `media_stories_controller.cpp:382-546`).
    #[must_use]
    pub fn compute(viewport: Rect) -> Self {
        let width = viewport.width().max(320.0);
        let height = viewport.height().max(360.0);

        let slider_total_h =
            STORIES_SLIDER_MARGIN_TOP + STORIES_SLIDER_HEIGHT + STORIES_SLIDER_MARGIN_BOTTOM;
        let outside_header_h = STORIES_HEADER_HEIGHT + slider_total_h + STORIES_SLIDER_OUTSIDE_SKIP;
        let min_height_for_outside = STORIES_FIELD_BOTTOM_MARGIN
            + outside_header_h
            + STORIES_MAX_HEIGHT
            + STORIES_FIELD_MIN_HEIGHT;

        let header_layout = if height >= min_height_for_outside {
            HeaderLayout::Outside
        } else {
            HeaderLayout::Normal
        };

        let top_skip = STORIES_FIELD_BOTTOM_MARGIN
            + if header_layout == HeaderLayout::Outside {
                outside_header_h
            } else {
                0.0
            };
        let bottom_skip = STORIES_FIELD_MIN_HEIGHT;
        let side_skip = if width > 560.0 {
            STORIES_SIDE_SKIP
        } else {
            (width * 0.08).max(16.0)
        };
        let max_width = (width - 2.0 * side_skip).max(180.0);
        let available_height = (height - top_skip - bottom_skip).max(220.0);
        let max_content_h = available_height.min(STORIES_MAX_HEIGHT);
        let now_w = max_content_h * STORIES_MAX_WIDTH / STORIES_MAX_HEIGHT;
        let content_w = now_w.min(max_width);
        let content_h = if content_w < now_w {
            content_w * STORIES_MAX_HEIGHT / STORIES_MAX_WIDTH
        } else {
            max_content_h
        };
        let added_top = (available_height - content_h) / 2.0;

        let content = Rect::from_min_size(
            Pos2::new(
                viewport.left() + (width - content_w) / 2.0,
                viewport.top() + added_top + top_skip,
            ),
            Vec2::new(content_w, content_h),
        );

        let (slider_outer, header) = if header_layout == HeaderLayout::Outside {
            let hdr = Rect::from_min_size(
                Pos2::new(content.left(), content.top() - outside_header_h),
                Vec2::new(content_w, STORIES_HEADER_HEIGHT),
            );
            let sld = Rect::from_min_size(
                Pos2::new(hdr.left(), hdr.bottom()),
                Vec2::new(content_w, slider_total_h),
            );
            (sld, hdr)
        } else {
            let sld = Rect::from_min_size(content.min, Vec2::new(content_w, slider_total_h));
            let hdr = Rect::from_min_size(
                Pos2::new(sld.left(), sld.bottom()),
                Vec2::new(content_w, STORIES_HEADER_HEIGHT),
            );
            (sld, hdr)
        };

        let slider = Rect::from_min_max(
            Pos2::new(
                slider_outer.left() + STORIES_SLIDER_MARGIN_X,
                slider_outer.top() + STORIES_SLIDER_MARGIN_TOP,
            ),
            Pos2::new(
                slider_outer.right() - STORIES_SLIDER_MARGIN_X,
                slider_outer.top() + STORIES_SLIDER_MARGIN_TOP + STORIES_SLIDER_HEIGHT,
            ),
        );

        let controls_w =
            (content_w + STORIES_CONTROLS_EXTEND * 2.0).max(STORIES_CONTROLS_MIN_WIDTH);
        let controls = Rect::from_min_size(
            Pos2::new(
                viewport.left() + (width - controls_w) / 2.0,
                content.bottom() + 10.0,
            ),
            Vec2::new(controls_w, 48.0),
        );

        // Sibling previews geometry matching `media_stories_controller.cpp:478-540`.
        let sides_available = width - content_w;
        let width_for_siblings = (sides_available - 2.0 * STORIES_FIELD_BOTTOM_MARGIN).max(0.0);
        let sibling_w_max = width_for_siblings / (2.0 * (1.0 - SIBLING_OUTSIDE_PART));
        let sibling_mult_max =
            SIBLING_MULTIPLIER_DEFAULT.max(STORIES_SIBLING_WIDTH_MIN / content_w.max(1.0));
        let sibling_mult = sibling_mult_max
            .min(SIBLING_MULTIPLIER_MAX)
            .min(sibling_w_max / content_w.max(1.0))
            .max(0.0);
        let sibling_size = Vec2::new(content_w * sibling_mult, content_h * sibling_mult);
        let sibling_top = viewport.top() + (height - sibling_size.y) / 2.0;
        let outside_max = (sibling_size.x * SIBLING_OUTSIDE_PART).round();
        let left_available = (content.left() - viewport.left()) - sibling_size.x;
        let x_desired = left_available / 3.0;
        let x_possible = x_desired.min(left_available - STORIES_CONTROL_SIZE);
        let x_left = x_possible.max(-outside_max);
        let x_right = width - sibling_size.x - x_left;

        let userpic_size = (sibling_size.x * SIBLING_USERPIC_RATIO).round().max(24.0);
        let inner_h = userpic_size * 1.6;
        let name_font_size =
            (STORIES_MAX_NAME_FONT_SIZE * content_h / STORIES_MAX_HEIGHT).clamp(12.0, 17.0);

        let make_sibling = |x: f32, is_left: bool| {
            let geom =
                Rect::from_min_size(Pos2::new(viewport.left() + x, sibling_top), sibling_size);
            let up = Rect::from_min_size(
                Pos2::new(
                    geom.left() + (geom.width() - userpic_size) / 2.0,
                    geom.top() + (geom.height() - inner_h) / 2.0,
                ),
                Vec2::splat(userpic_size),
            );
            let skip_small = name_font_size;
            let skip_big = skip_small - x_left.min(0.0);
            let name_left = if is_left { skip_big } else { skip_small };
            let name_w = (geom.width() - skip_small - skip_big).max(20.0);
            let name_rect = Rect::from_min_size(
                Pos2::new(geom.left() + name_left, up.bottom() + 8.0),
                Vec2::new(name_w, name_font_size + 8.0),
            );
            SiblingCardLayout {
                geometry: geom,
                userpic: up,
                name_rect,
                name_font_size,
            }
        };

        let sibling_left = make_sibling(x_left, true);
        let sibling_right = make_sibling(x_right, false);

        let arrow_y = content.center().y - STORIES_CONTROL_SIZE / 2.0;
        let prev_arrow = Rect::from_min_size(
            Pos2::new(content.left() - STORIES_CONTROL_SIZE, arrow_y),
            Vec2::splat(STORIES_CONTROL_SIZE),
        );
        let next_arrow = Rect::from_min_size(
            Pos2::new(content.right(), arrow_y),
            Vec2::splat(STORIES_CONTROL_SIZE),
        );
        let close_button = Rect::from_min_size(
            Pos2::new(viewport.right() - 52.0, viewport.top() + 14.0),
            Vec2::splat(38.0),
        );

        Self {
            viewport,
            header_layout,
            content,
            slider,
            header,
            controls,
            sibling_left,
            sibling_right,
            prev_arrow,
            next_arrow,
            close_button,
        }
    }

    /// Returns `true` if clicking `pos` falls in the outer dismiss backdrop
    /// (`Controller::closeByClickAt` in `media_stories_controller.cpp:604-611`).
    #[must_use]
    pub fn close_by_click_at(&self, pos: Pos2) -> bool {
        pos.x < self.content.left() - STORIES_CONTROL_SIZE
            || pos.x > self.content.right() + STORIES_CONTROL_SIZE
    }

    /// Classifies a tap inside `self.content` into left-prev (left 30%) or right-next (right 70%).
    #[must_use]
    pub fn is_left_tap(&self, pos: Pos2) -> bool {
        let rel_x = (pos.x - self.content.left()) / self.content.width().max(1.0);
        rel_x < 0.30
    }

    /// Converts a screen position inside `self.content` to normalized `(nx, ny)` in `[0.0, 1.0]`.
    #[must_use]
    pub fn normalized_point(&self, pos: Pos2) -> Option<(f64, f64)> {
        if !self.content.contains(pos) {
            return None;
        }
        let nx = f64::from(pos.x - self.content.left()) / f64::from(self.content.width().max(1.0));
        let ny = f64::from(pos.y - self.content.top()) / f64::from(self.content.height().max(1.0));
        Some((nx.clamp(0.0, 1.0), ny.clamp(0.0, 1.0)))
    }
}

// ---------------------------------------------------------------------------
// Progress Slider Segments (`media_stories_slider.cpp`)
// ---------------------------------------------------------------------------

/// Visual state of a single top slider segment.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct SliderSegmentRect {
    /// Segment bounding rectangle.
    pub rect: Rect,
    /// Filled active portion inside `rect` (`0.0` = upcoming, `1.0` = completed, `0..1` = active).
    pub fill_ratio: f32,
    /// Whether this segment is the active story.
    pub is_active: bool,
}

/// Computes slider segment rectangles matching `Slider::layout` (`media_stories_slider.cpp:90-112`).
#[must_use]
pub fn compute_slider_segments(bar_rect: Rect, state: SliderState) -> Vec<SliderSegmentRect> {
    if state.video_stream || bar_rect.width() < STORIES_SLIDER_HEIGHT {
        return Vec::new();
    }
    let single = bar_rect.height().max(STORIES_SLIDER_HEIGHT);
    let skip = STORIES_SLIDER_SKIP;
    let width = bar_rect.width();
    let max_segments = ((width + skip) / (single + skip)).floor().max(1.0) as usize;
    let count = state.total.clamp(1, max_segments);
    let one = (width - ((count - 1) as f32) * skip) / (count as f32);

    let mut out = Vec::with_capacity(count);
    let mut left = bar_rect.left();
    for i in 0..count {
        let rect = Rect::from_min_size(Pos2::new(left, bar_rect.top()), Vec2::new(one, single));
        let fill_ratio = if i < state.index {
            1.0
        } else if i == state.index {
            state.active_progress.clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        out.push(SliderSegmentRect {
            rect,
            fill_ratio,
            is_active: i == state.index,
        });
        left += one + skip;
    }
    out
}

// ---------------------------------------------------------------------------
// Interactive Area Projection & Weather Helpers
// ---------------------------------------------------------------------------

/// Projects a normalized [`StoryArea`] into pixel coordinates within `content_rect`
/// (`Controller::rebuildActiveAreas` in `media_stories_controller.cpp:548-567`).
///
/// Returns `(unrotated_rect, corner_radius_px)`.
#[must_use]
pub fn project_area_rect(area: StoryArea, content_rect: Rect) -> (Rect, f32) {
    let w = f64::from(content_rect.width());
    let h = f64::from(content_rect.height());
    let rect = Rect::from_min_size(
        Pos2::new(
            content_rect.left() + (area.geometry.x * w).round() as f32,
            content_rect.top() + (area.geometry.y * h).round() as f32,
        ),
        Vec2::new(
            (area.geometry.width * w).round().max(24.0) as f32,
            (area.geometry.height * h).round().max(20.0) as f32,
        ),
    );
    let radius = if area.radius > 0.0 {
        (w * area.radius / 100.0) as f32
    } else {
        (rect.height() * 0.25).clamp(6.0, 14.0)
    };
    (rect, radius)
}

/// Computes the 4 rotated corner vertices of `rect` around its center by `angle_deg` degrees
/// clockwise, matching `Rotated()` in `media_stories_controller.cpp:131-142`.
#[must_use]
pub fn rotated_rect_corners(rect: Rect, angle_deg: f64) -> [Pos2; 4] {
    let center = rect.center();
    let ox = f64::from(center.x);
    let oy = f64::from(center.y);
    let raw = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ];
    raw.map(|p| {
        let (rx, ry) = rotate_point(f64::from(p.x), f64::from(p.y), ox, oy, angle_deg);
        Pos2::new(rx as f32, ry as f32)
    })
}

/// Computes a rounded-rectangle polygon rotated by `angle_deg` degrees around `rect.center()`.
#[must_use]
pub fn rotated_rounded_rect_points(
    rect: Rect,
    radius: f32,
    angle_deg: f64,
    arc_steps: usize,
) -> Vec<Pos2> {
    let r = radius.clamp(0.0, rect.width().min(rect.height()) / 2.0);
    let steps = arc_steps.max(2);
    let corners = [
        (Pos2::new(rect.right() - r, rect.top() + r), -PI / 2.0, 0.0),
        (
            Pos2::new(rect.right() - r, rect.bottom() - r),
            0.0,
            PI / 2.0,
        ),
        (Pos2::new(rect.left() + r, rect.bottom() - r), PI / 2.0, PI),
        (
            Pos2::new(rect.left() + r, rect.top() + r),
            PI,
            3.0 * PI / 2.0,
        ),
    ];
    let center = rect.center();
    let ox = f64::from(center.x);
    let oy = f64::from(center.y);
    let mut pts = Vec::with_capacity(corners.len() * (steps + 1));
    for (c, start_a, end_a) in corners {
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let a = start_a + (end_a - start_a) * t;
            let px = f64::from(c.x + r * a.cos());
            let py = f64::from(c.y + r * a.sin());
            let (rx, ry) = rotate_point(px, py, ox, oy, angle_deg);
            pts.push(Pos2::new(rx as f32, ry as f32));
        }
    }
    pts
}

/// Chooses contrasting black or white foreground text for a Weather area background
/// (`ChooseWeatherFg` in `media_stories_reactions.cpp:205-210`).
#[must_use]
pub fn choose_weather_fg(bg: Color32) -> Color32 {
    let lum = 0.2126 * (f32::from(bg.r()) / 255.0)
        + 0.7152 * (f32::from(bg.g()) / 255.0)
        + 0.0722 * (f32::from(bg.b()) / 255.0);
    if lum > 0.705 {
        Color32::BLACK
    } else {
        Color32::WHITE
    }
}

/// Converts a packed ARGB `u32` (from `WeatherArea::color`) to `egui::Color32`.
#[must_use]
pub fn argb_u32_to_color32(argb: u32) -> Color32 {
    let a = ((argb >> 24) & 0xFF) as u8;
    let r = ((argb >> 16) & 0xFF) as u8;
    let g = ((argb >> 8) & 0xFF) as u8;
    let b = (argb & 0xFF) as u8;
    let effective_a = if a == 0 { 230 } else { a };
    Color32::from_rgba_unmultiplied(r, g, b, effective_a)
}

/// Formats temperature from `millicelsius` in Celsius or Fahrenheit.
#[must_use]
pub fn format_temperature(millicelsius: i32, celsius: bool) -> String {
    let c = f64::from(millicelsius) / 1000.0;
    if celsius {
        format!("{}°C", c.round() as i32)
    } else {
        let f = c * 9.0 / 5.0 + 32.0;
        format!("{}°F", f.round() as i32)
    }
}

// ---------------------------------------------------------------------------
// Timestamp & Counter Formatting (`media_stories_header.cpp`)
// ---------------------------------------------------------------------------

/// Formats a relative story timestamp matching `ComposeTimestamp`
/// (`media_stories_header.cpp:204-246`).
#[must_use]
pub fn format_story_timestamp(when: TimeId, now: TimeId) -> String {
    let diff = (now - when).max(0);
    let minutes = diff / 60;
    if minutes == 0 {
        return "just now".to_owned();
    }
    if minutes < 60 {
        return format!("{minutes}m ago");
    }
    let hours = diff / 3600;
    if hours < 12 {
        return format!("{hours}h ago");
    }
    let when_day = when.div_euclid(86_400);
    let now_day = now.div_euclid(86_400);
    let tod = when.rem_euclid(86_400);
    let hh = tod / 3600;
    let mm = (tod % 3600) / 60;
    if when_day == now_day {
        format!("today at {hh:02}:{mm:02}")
    } else if when_day + 1 == now_day {
        format!("yesterday at {hh:02}:{mm:02}")
    } else {
        let days_ago = (now_day - when_day).max(2);
        format!("{days_ago}d ago")
    }
}

/// Formats compact counts (`1.2K`, `3.4M`, or exact integer) for views/reactions pills.
#[must_use]
pub fn format_compact_count(count: i32) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", f64::from(count) / 1_000_000.0)
    } else if count >= 10_000 {
        format!("{}K", count / 1_000)
    } else if count >= 1_000 {
        format!("{:.1}K", f64::from(count) / 1_000.0)
    } else {
        count.max(0).to_string()
    }
}

// ---------------------------------------------------------------------------
// Dialogs Stories Strip Metrics & Interpolation (`dialogs_stories_list.cpp` & `dialogs.style`)
// ---------------------------------------------------------------------------

/// Exact tdesktop layout metrics from `dialogsStories` and `dialogsStoriesFull` (`dialogs.style:682-711`).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct StoriesStripMetrics {
    /// Collapsed strip height (`dialogsStories.height = 35px`).
    pub small_height: f32,
    /// Collapsed avatar diameter (`dialogsStories.photo = 21px`).
    pub small_photo: f32,
    /// Collapsed horizontal overlap shift (`dialogsStories.shift = 16px`).
    pub small_shift: f32,
    /// Collapsed left padding (`dialogsStories.left = 4px`).
    pub small_left: f32,
    /// Collapsed ring stroke width (`dialogsStories.lineTwice / 2 = 1.5px`).
    pub small_line: f32,
    /// Collapsed margin around each avatar (`dialogsStories.photoLeft = 4px`).
    pub small_photo_left: f32,
    /// Collapsed top margin for avatar (`dialogsStories.photoTop = 4px`).
    pub small_photo_top: f32,
    /// Expanded strip height (`dialogsStoriesFull.height = 77px`).
    pub full_height: f32,
    /// Expanded avatar diameter (`dialogsStoriesFull.photo = 42px`).
    pub full_photo: f32,
    /// Expanded horizontal margin around each avatar (`dialogsStoriesFull.photoLeft = 10px`).
    pub full_photo_left: f32,
    /// Expanded top margin for avatar (`dialogsStoriesFull.photoTop = 9px`).
    pub full_photo_top: f32,
    /// Expanded left padding (`dialogsStoriesFull.left = 4px`).
    pub full_left: f32,
    /// Expanded unread ring stroke (`dialogsStoriesFull.lineTwice / 2 = 2.0px`).
    pub full_line_unread: f32,
    /// Expanded read ring stroke (`dialogsStoriesFull.lineReadTwice / 2 = 1.0px`).
    pub full_line_read: f32,
    /// Expanded name Y offset (`dialogsStoriesFull.nameTop = 56px`).
    pub full_name_top: f32,
}

impl Default for StoriesStripMetrics {
    fn default() -> Self {
        Self {
            small_height: 35.0,
            small_photo: 21.0,
            small_shift: 16.0,
            small_left: 4.0,
            small_line: 1.5,
            small_photo_left: 4.0,
            small_photo_top: 4.0,
            full_height: 77.0,
            full_photo: 42.0,
            full_photo_left: 10.0,
            full_photo_top: 9.0,
            full_left: 4.0,
            full_line_unread: 2.0,
            full_line_read: 1.0,
            full_name_top: 56.0,
        }
    }
}

impl StoriesStripMetrics {
    /// Width of a single item cell in expanded mode (`photoLeft * 2 + photo = 62px`).
    #[must_use]
    pub const fn single_full_width(&self) -> f32 {
        self.full_photo_left * 2.0 + self.full_photo
    }
}

/// Collapsed strip geometry inside the chat-list search field (`List::countSmallGeometry` with
/// `setLayoutConstraints({filterLeft + filterWidth, filterTop + added}, al_right, ...)`):
/// right-aligned to the field's right edge and centred in its height.
#[must_use]
pub fn collapsed_strip_geometry(
    metrics: &StoriesStripMetrics,
    field_rect: Rect,
    count: usize,
) -> Rect {
    let shown = count.clamp(1, SMALL_THUMBS_SHOWN) as f32;
    let width = 2.0 * metrics.small_left
        + 2.0 * metrics.small_photo_left
        + metrics.small_photo
        + (shown - 1.0) * metrics.small_shift;
    let height = 2.0 * metrics.small_photo_top + metrics.small_photo;
    // `added = (st::dialogsFilter.heightMin - storiesHeight) / 2`, integer pixels.
    let added = ((field_rect.height() - height) / 2.0).floor();
    Rect::from_min_size(
        Pos2::new(field_rect.right() - width, field_rect.top() + added),
        Vec2::new(width, height),
    )
}

/// Centre of the collapsed userpic in `slot` (0 = leftmost, painted on top) inside `geometry`.
#[must_use]
pub fn collapsed_photo_center(metrics: &StoriesStripMetrics, geometry: Rect, slot: usize) -> Pos2 {
    let radius = metrics.small_photo / 2.0;
    Pos2::new(
        geometry.left()
            + metrics.small_left
            + metrics.small_photo_left
            + radius
            + slot as f32 * metrics.small_shift,
        geometry.top() + metrics.small_photo_top + radius,
    )
}

/// Decelerate easing from `dialogs_stories_list.cpp:48-50`.
#[must_use]
pub fn decelerate(t: f64) -> f64 {
    let c = t.clamp(0.0, 1.0);
    1.0 - (1.0 - c) * (1.0 - c)
}

/// Ease-out quint from `dialogs_stories_list.cpp:51-54`.
#[must_use]
pub fn ease_out_quint(t: f64) -> f64 {
    let u = 1.0 - t.clamp(0.0, 1.0);
    1.0 - u * u * u * u * u
}

/// Interpolated strip geometry for rendering `Small`, `Changing`, or `Full` states.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct StripRenderLayout {
    /// Current strip height in pixels.
    pub height: f32,
    /// Normalized visual expansion ratio in `[0.0, 1.0]`.
    pub expand_ratio: f32,
    /// Current avatar diameter in pixels.
    pub photo_size: f32,
    /// Title opacity in collapsed mode (`1.0` when small, fades out on expand).
    pub title_opacity: f32,
    /// Name label opacity beneath avatars (`0.0` when small, fades in on expand).
    pub name_opacity: f32,
    /// Width of one item cell in expanded mode (distributed evenly if fewer items fit).
    pub single_full: f32,
    /// Left padding of first item in expanded mode.
    pub left_full: f32,
    /// Number of collapsed thumbs shown (`<= SMALL_THUMBS_SHOWN`).
    pub small_count: usize,
}

/// Computes interpolated strip layout matching `List::computeLayout` (`dialogs_stories_list.cpp:326-400`).
#[must_use]
pub fn compute_strip_layout(
    state: &DialogStoriesBarState,
    metrics: &StoriesStripMetrics,
    viewport_width: f32,
    anim_progress: f64,
) -> StripRenderLayout {
    let t = decelerate(anim_progress);
    let ratio = state.visual_expand_ratio(t).max(t);
    let ratio_f32 = ratio as f32;
    let height = metrics.small_height + (metrics.full_height - metrics.small_height) * ratio_f32;
    let photo_size = metrics.small_photo + (metrics.full_photo - metrics.small_photo) * ratio_f32;

    let items_count = state.content().elements.len();
    let single_min = metrics.single_full_width();
    let total_full = metrics.full_left + single_min * (items_count as f32);
    let (skip_side, skip_between) = if items_count > 0 && total_full < viewport_width {
        let side = ((viewport_width - total_full) / ((items_count + 1) as f32)).min(14.0);
        let between = if items_count > 1 {
            ((viewport_width - total_full - 2.0 * side) / ((items_count - 1) as f32)).min(18.0)
        } else {
            side
        };
        (side, between)
    } else {
        (0.0, 0.0)
    };
    let single_full = single_min + skip_between;
    let left_full = metrics.full_left - (state.scroll_left() as f32) + skip_side;
    let small_count = state.small_visible_indices().len().min(SMALL_THUMBS_SHOWN);

    // Reference hysteresis constants so tests/layout stay synced with tg-stories.
    let _ = (EXPAND_AFTER_RATIO, COLLAPSE_AFTER_RATIO, FRICTION_RATIO);

    StripRenderLayout {
        height,
        expand_ratio: ratio_f32,
        photo_size,
        title_opacity: (1.0 - ratio_f32 * 1.4).clamp(0.0, 1.0),
        name_opacity: ((ratio_f32 - 0.35) / 0.65).clamp(0.0, 1.0),
        single_full,
        left_full,
        small_count,
    }
}

/// Computes `(start_angle_rad, end_angle_rad)` clockwise from 12 o'clock for segment `index`
/// out of `count` segments around a circular userpic ring.
#[must_use]
pub fn ring_segment_arc_angles(count: u32, index: u32) -> (f32, f32) {
    if count <= 1 {
        return (-PI / 2.0, 3.0 * PI / 2.0);
    }
    let full = 2.0 * PI;
    let step = full / (count as f32);
    let gap = (0.14_f32).min(step * 0.28);
    let start = -PI / 2.0 + (index as f32) * step + gap / 2.0;
    let end = -PI / 2.0 + ((index + 1) as f32) * step - gap / 2.0;
    (start, end.max(start + 0.04))
}
