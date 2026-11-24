//! Media viewer, playback controls, PiP, and audio player styling, theme resolution, and icons.
//!
//! Port origin: `Telegram/SourceFiles/media/view/media_view.style`,
//! `Telegram/SourceFiles/media/player/media_player.style`, and `resources/colors.palette`.

use std::sync::OnceLock;

use egui::{Color32, CornerRadius, Painter, Pos2, Rect, Stroke, Vec2};
use tg_ui::icons::paint_style_icon;
use tg_ui::tg_style::{Color as StyleColor, ColorResolver, Icon, st};
use tg_ui::tg_theme::{EmbeddedType, Palette as ThemePalette};
use tg_ui::theme::{Colors, ThemeManager, egui_color_to_style};

use tg_app_core::stand_in::theme::Palette;

// ── Media Viewer Palette (`resources/colors.palette`) ────────────────────────

/// Media viewer dimmed backdrop (`mediaviewBg: #222222eb`).
pub const MEDIAVIEW_BG: Color32 = Color32::from_rgba_unmultiplied_const(0x22, 0x22, 0x22, 0xEB);

/// Media viewer fullscreen video backdrop (`mediaviewVideoBg: #000000`).
pub const MEDIAVIEW_VIDEO_BG: Color32 = Color32::from_rgb(0x00, 0x00, 0x00);

/// Overlay control button backdrop (`mediaviewControlBg: #0000003c`).
pub const MEDIAVIEW_CONTROL_BG: Color32 =
    Color32::from_rgba_unmultiplied_const(0x00, 0x00, 0x00, 0x3C);

/// Overlay control hover backdrop.
pub const MEDIAVIEW_CONTROL_BG_OVER: Color32 =
    Color32::from_rgba_unmultiplied_const(0x00, 0x00, 0x00, 0x6E);

/// Overlay control foreground (`mediaviewControlFg: #ffffff`).
pub const MEDIAVIEW_CONTROL_FG: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);

/// Secondary metadata foreground (date, counter).
pub const MEDIAVIEW_SUB_FG: Color32 = Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 0xB8);

/// Caption bubble background (`mediaviewCaptionBg: #11111180`).
pub const MEDIAVIEW_CAPTION_BG: Color32 =
    Color32::from_rgba_unmultiplied_const(0x11, 0x11, 0x11, 0x99);

/// Caption text foreground (`mediaviewCaptionFg: #ffffff`).
pub const MEDIAVIEW_CAPTION_FG: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);

/// Caption link foreground (`mediaviewTextLinkFg: #4db8ff`).
pub const MEDIAVIEW_TEXT_LINK_FG: Color32 = Color32::from_rgb(0x4D, 0xB8, 0xFF);

/// Save toast and playback controls pill background (`mediaviewSaveMsgBg: #2c3033e5`).
pub const MEDIAVIEW_SAVE_MSG_BG: Color32 =
    Color32::from_rgba_unmultiplied_const(0x2C, 0x30, 0x33, 0xE5);

/// Save toast foreground (`mediaviewSaveMsgFg: #ffffff`).
pub const MEDIAVIEW_SAVE_MSG_FG: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);

/// Context menu background (`mediaviewMenuBg: #383838`).
pub const MEDIAVIEW_MENU_BG: Color32 = Color32::from_rgb(0x38, 0x38, 0x38);

/// Context menu hovered item background (`mediaviewMenuBgOver: #505050`).
pub const MEDIAVIEW_MENU_BG_OVER: Color32 = Color32::from_rgb(0x50, 0x50, 0x50);

/// Context menu foreground (`mediaviewMenuFg: #ffffff`).
pub const MEDIAVIEW_MENU_FG: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);

/// Attention/destructive action text inside dark menus.
pub const MEDIAVIEW_MENU_DANGER_FG: Color32 = Color32::from_rgb(0xFF, 0x6B, 0x6B);

/// Video playback progress played fill (`mediaviewPlaybackActive: #c7c7c7`).
pub const MEDIAVIEW_PLAYBACK_ACTIVE: Color32 = Color32::from_rgb(0xC7, 0xC7, 0xC7);

/// Video playback progress hovered fill (`mediaviewPlaybackActiveOver: #ffffff`).
pub const MEDIAVIEW_PLAYBACK_ACTIVE_OVER: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);

/// Video playback progress track (`mediaviewPlaybackInactive: #252525`).
pub const MEDIAVIEW_PLAYBACK_INACTIVE: Color32 = Color32::from_rgb(0x25, 0x25, 0x25);

/// Video playback buffered track (`mediaviewPlaybackInactiveOver: #474747`).
pub const MEDIAVIEW_PLAYBACK_BUFFERED: Color32 = Color32::from_rgb(0x47, 0x47, 0x47);

/// Video playback time text (`mediaviewPlaybackProgressFg: #ffffffc7`).
pub const MEDIAVIEW_PLAYBACK_PROGRESS_FG: Color32 =
    Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 0xC7);

/// Video playback icon foreground (`mediaviewPlaybackIconFg: #c7c7c7`).
pub const MEDIAVIEW_PLAYBACK_ICON_FG: Color32 = MEDIAVIEW_PLAYBACK_ACTIVE;

/// Video playback icon hover foreground (`mediaviewPlaybackIconFgOver: #ffffff`).
pub const MEDIAVIEW_PLAYBACK_ICON_FG_OVER: Color32 = MEDIAVIEW_PLAYBACK_ACTIVE_OVER;

/// PiP controls foreground (`mediaviewPipControlsFg: #ffffffd9`).
pub const MEDIAVIEW_PIP_CONTROLS_FG: Color32 =
    Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 0xD9);

/// PiP controls hover foreground (`mediaviewPipControlsFgOver: #ffffff`).
pub const MEDIAVIEW_PIP_CONTROLS_FG_OVER: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);

/// PiP playback active fill (`mediaviewPipPlaybackActive: #ffffffda`).
pub const MEDIAVIEW_PIP_PLAYBACK_ACTIVE: Color32 =
    Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 0xDA);

/// PiP playback inactive track (`mediaviewPipPlaybackInactive: #ffffff26`).
pub const MEDIAVIEW_PIP_PLAYBACK_INACTIVE: Color32 =
    Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 0x26);

/// Transparent PNG checkerboard light tile (`mediaviewTransparentBg: #ffffff`).
pub const MEDIAVIEW_TRANSPARENT_BG: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);

/// Transparent PNG checkerboard dark tile (`mediaviewTransparentFg: #cccccc`).
pub const MEDIAVIEW_TRANSPARENT_FG: Color32 = Color32::from_rgb(0xCC, 0xCC, 0xCC);

/// Audio player active accent (`mediaPlayerActiveFg: #40a7e3`).
pub const MEDIA_PLAYER_ACTIVE_FG: Color32 = Color32::from_rgb(0x40, 0xA7, 0xE3);

/// Audio player inactive slider track (`mediaPlayerInactiveFg: #e1eaef`).
pub const MEDIA_PLAYER_INACTIVE_FG: Color32 = Color32::from_rgb(0xE1, 0xEA, 0xEF);

/// Audio player disabled/loading slider track (`mediaPlayerDisabledFg: #9dd1ef`).
pub const MEDIA_PLAYER_DISABLED_FG: Color32 = Color32::from_rgb(0x9D, 0xD1, 0xEF);

// ── Layout Metrics (`media_view.style` & `media_player.style` via `tg_style::st`) ──

/// Floating video playback controls bar size (`st::mediaviewControllerSize`).
pub const MEDIAVIEW_CONTROLLER_WIDTH: f32 = st::mediaview_controller_size.width.0 as f32;
/// Floating video playback controls bar height (`st::mediaviewControllerSize`).
pub const MEDIAVIEW_CONTROLLER_HEIGHT: f32 = st::mediaview_controller_size.height.0 as f32;
/// Floating video playback controls corner radius (`st::mediaviewControllerRadius`).
pub const MEDIAVIEW_CONTROLLER_RADIUS: f32 = st::mediaview_controller_radius.0 as f32;
/// Caption bubble corner radius (`st::mediaviewCaptionRadius`).
pub const MEDIAVIEW_CAPTION_RADIUS: f32 = st::mediaview_caption_radius.0 as f32;
/// Caption inner horizontal padding (`st::mediaviewCaptionPadding`).
pub const MEDIAVIEW_CAPTION_PAD_X: f32 = st::mediaview_caption_padding.left.0 as f32;
/// Caption inner vertical padding (`st::mediaviewCaptionPadding`).
pub const MEDIAVIEW_CAPTION_PAD_Y: f32 = st::mediaview_caption_padding.top.0 as f32;
/// Caption margin (`st::mediaviewCaptionMargin`).
pub const MEDIAVIEW_CAPTION_MARGIN: f32 = st::mediaview_caption_margin.width.0 as f32;
/// Top audio player bar height (`st::mediaPlayerHeight`).
pub const MEDIA_PLAYER_HEIGHT: f32 = st::media_player_height.0 as f32;
/// Top audio player bar wide threshold (`st::mediaPlayerWideWidth`).
pub const MEDIA_PLAYER_WIDE_WIDTH: f32 = st::media_player_wide_width.0 as f32;
/// Round video float circle diameter (`st::mediaPlayerFloatSize`).
pub const MEDIA_PLAYER_FLOAT_SIZE: f32 = st::media_player_float_size.0 as f32;
/// Round video float outer margin (`st::mediaPlayerFloatMargin`).
pub const MEDIA_PLAYER_FLOAT_MARGIN: f32 = st::media_player_float_margin.0 as f32;
/// Controls auto-hide idle wait (`st::mediaviewWaitHide` = 1100 ms).
pub const MEDIAVIEW_WAIT_HIDE_MS: u64 = st::mediaview_wait_hide as u64;
/// Controls fade-out duration (`st::mediaviewHideDuration` = 600 ms).
pub const MEDIAVIEW_HIDE_DURATION_MS: u64 = st::mediaview_hide_duration as u64;
/// Controls fade-in duration (`st::mediaviewShowDuration` = 200 ms).
pub const MEDIAVIEW_SHOW_DURATION_MS: u64 = st::mediaview_show_duration as u64;
/// Save toast shown duration (`st::mediaviewSaveMsgShown` = 2000 ms).
pub const MEDIAVIEW_SAVE_MSG_SHOWN_MS: u64 = st::mediaview_save_msg_shown as u64;

// ── ThemeManager Palette Adapter ─────────────────────────────────────────────

fn cached_theme_palette(is_night: bool) -> &'static ThemePalette {
    static DAY: OnceLock<ThemePalette> = OnceLock::new();
    static NIGHT: OnceLock<ThemePalette> = OnceLock::new();
    if is_night {
        NIGHT.get_or_init(|| {
            ThemeManager::with_embedded(EmbeddedType::Night)
                .palette()
                .clone()
        })
    } else {
        DAY.get_or_init(|| {
            ThemeManager::with_embedded(EmbeddedType::Default)
                .palette()
                .clone()
        })
    }
}

/// Live `tg-theme` palette adapter for all media viewer, PiP, audio player, and float widgets.
#[derive(Clone, Copy)]
pub struct MediaColors<'a> {
    theme_palette: &'a ThemePalette,
    is_night: bool,
}

impl<'a> MediaColors<'a> {
    /// Resolves Day or Night embedded `tg-theme` palette via [`ThemeManager`].
    #[must_use]
    pub fn for_night(is_night: bool) -> Self {
        Self {
            theme_palette: cached_theme_palette(is_night),
            is_night,
        }
    }

    /// Resolves media colors from a live [`ThemeManager`].
    #[must_use]
    pub fn from_theme(theme: &'a ThemeManager) -> Self {
        Self {
            theme_palette: theme.palette(),
            is_night: theme.is_dark(),
        }
    }

    /// Resolves media colors matching the stand-in app [`Palette`] (Day vs Night).
    #[must_use]
    pub fn from_palette(palette: &Palette) -> Self {
        Self::for_night(palette.window_bg.r() < 128)
    }

    /// Resolves media colors matching the active `egui::Context` theme (Day vs Night).
    #[must_use]
    pub fn from_ctx(ctx: &egui::Context) -> Self {
        Self::for_night(ctx.global_style().visuals.dark_mode)
    }

    /// Returns `true` when Night theme palette is active.
    #[must_use]
    pub const fn is_night(&self) -> bool {
        self.is_night
    }

    /// Looks up a named `colors.palette` token or falls back to `default`.
    #[must_use]
    pub fn color_or(&self, name: &str, default: Color32) -> Color32 {
        self.theme_palette.get(name).unwrap_or(default)
    }
}

impl Colors for MediaColors<'_> {
    fn get(&self, name: &str) -> Option<Color32> {
        self.theme_palette.get(name)
    }
}

impl ColorResolver for MediaColors<'_> {
    fn resolve(&self, name: &str) -> Option<StyleColor> {
        self.theme_palette
            .get(name)
            .map(egui_color_to_style)
            .or_else(|| self.theme_palette.resolve(name))
    }
}

struct AlphaColorResolver<'a> {
    inner: &'a dyn ColorResolver,
    alpha: f32,
}

impl ColorResolver for AlphaColorResolver<'_> {
    fn resolve(&self, name: &str) -> Option<StyleColor> {
        self.inner.resolve(name).map(|c| {
            let scaled =
                ((f32::from(c.a) * self.alpha.clamp(0.0, 1.0)).round() as u32).min(255) as u8;
            StyleColor::rgba(c.r, c.g, c.b, scaled)
        })
    }
}

/// Paints a `tg-style` [`Icon`] centered inside `rect` using the live [`ColorResolver`].
pub fn paint_centered_style_icon(
    _ctx: &egui::Context,
    painter: &Painter,
    icon: &Icon,
    rect: Rect,
    resolver: &dyn ColorResolver,
) {
    let _ = paint_style_icon(painter, rect, *icon, resolver, None);
}

/// Paints a `tg-style` [`Icon`] centered inside `rect` with multiplied `alpha` opacity.
pub fn paint_centered_style_icon_alpha(
    _ctx: &egui::Context,
    painter: &Painter,
    icon: &Icon,
    rect: Rect,
    resolver: &dyn ColorResolver,
    alpha: f32,
) {
    let wrapped = AlphaColorResolver {
        inner: resolver,
        alpha,
    };
    let _ = paint_style_icon(painter, rect, *icon, &wrapped, None);
}

/// Paints a `tg-style` [`Icon`] centered inside `rect` tinted with an explicit `Color32`.
pub fn paint_centered_style_icon_tinted(
    _ctx: &egui::Context,
    painter: &Painter,
    icon: &Icon,
    rect: Rect,
    resolver: &dyn ColorResolver,
    tint: Color32,
) {
    let _ = paint_style_icon(painter, rect, *icon, resolver, Some(tint));
}

/// Multiplies a color's alpha channel by `opacity` in `[0.0, 1.0]`.
#[must_use]
pub fn with_opacity(color: Color32, opacity: f32) -> Color32 {
    let [r, g, b, a] = color.to_srgba_unmultiplied();
    let scaled = ((f32::from(a) * opacity.clamp(0.0, 1.0)).round() as u32).min(255) as u8;
    Color32::from_rgba_unmultiplied(r, g, b, scaled)
}

/// Formats a duration in seconds using tdesktop's `FormatDurationText` rules (`m:ss` or `h:mm:ss`).
#[must_use]
pub fn format_duration_secs(total_secs: i64) -> String {
    let secs = total_secs.max(0);
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let rem_secs = secs % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{rem_secs:02}")
    } else {
        format!("{minutes}:{rem_secs:02}")
    }
}

/// Formats a remaining duration prefixed with Unicode minus (`−`, `U+2212`) matching `media_view_playback_controls.cpp`.
#[must_use]
pub fn format_remaining_secs(remaining_secs: i64) -> String {
    format!("\u{2212}{}", format_duration_secs(remaining_secs.max(0)))
}

/// Volume speaker icon level (`mediaviewVolumeIcon0` / `1` / `2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeIconLevel {
    /// Muted (`volume <= 0.0`).
    Off,
    /// Low/medium volume (`0.0 < volume < 0.5`).
    Small,
    /// High volume (`volume >= 0.5`).
    Full,
}

impl VolumeIconLevel {
    /// Maps a normalized volume in `[0.0, 1.0]` to an icon level (matches `media_view_playback_controls.cpp:269-273`).
    #[must_use]
    pub fn from_volume(volume: f64) -> Self {
        if volume <= 0.0 {
            Self::Off
        } else if volume < 0.5 {
            Self::Small
        } else {
            Self::Full
        }
    }
}

/// Paints a checkerboard pattern behind transparent images (`mediaviewTransparentBg` / `Fg`).
pub fn paint_transparent_checkerboard(
    painter: &Painter,
    rect: Rect,
    tile_size: f32,
    colors: &MediaColors<'_>,
) {
    let tile = tile_size.max(6.0);
    let bg = colors.color_or("mediaviewTransparentBg", MEDIAVIEW_TRANSPARENT_BG);
    let fg = colors.color_or("mediaviewTransparentFg", MEDIAVIEW_TRANSPARENT_FG);
    painter.rect_filled(rect, CornerRadius::ZERO, bg);
    let cols = ((rect.width() / tile).ceil() as usize).min(256);
    let rows = ((rect.height() / tile).ceil() as usize).min(256);
    for row in 0..rows {
        for col in 0..cols {
            if (row + col) % 2 == 1 {
                let min = Pos2::new(
                    rect.min.x + col as f32 * tile,
                    rect.min.y + row as f32 * tile,
                );
                let max = Pos2::new(
                    (min.x + tile).min(rect.max.x),
                    (min.y + tile).min(rect.max.y),
                );
                painter.rect_filled(Rect::from_min_max(min, max), CornerRadius::ZERO, fg);
            }
        }
    }
}

/// Paints a right-pointing play triangle inside `rect` (fallback helper).
pub fn paint_play_icon(painter: &Painter, rect: Rect, color: Color32) {
    let c = rect.center();
    let r = rect.width().min(rect.height()) * 0.34;
    let pts = vec![
        Pos2::new(c.x - r * 0.65, c.y - r),
        Pos2::new(c.x + r * 0.95, c.y),
        Pos2::new(c.x - r * 0.65, c.y + r),
    ];
    painter.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
}

/// Paints a two-bar pause icon inside `rect` (fallback helper).
pub fn paint_pause_icon(painter: &Painter, rect: Rect, color: Color32) {
    let c = rect.center();
    let h = rect.height() * 0.56;
    let bar_w = (rect.width() * 0.16).max(2.5);
    let gap = (rect.width() * 0.16).max(3.0);
    let left = Rect::from_center_size(
        Pos2::new(c.x - (bar_w + gap) * 0.5, c.y),
        Vec2::new(bar_w, h),
    );
    let right = Rect::from_center_size(
        Pos2::new(c.x + (bar_w + gap) * 0.5, c.y),
        Vec2::new(bar_w, h),
    );
    painter.rect_filled(left, CornerRadius::same(1), color);
    painter.rect_filled(right, CornerRadius::same(1), color);
}
