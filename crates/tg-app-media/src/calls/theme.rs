//! Call UI colors, layout constants, and custom vector icon painters.
//!
//! Port of `Telegram/Resources/colors.palette` (calls section) and
//! `Telegram/SourceFiles/calls/calls.style`.

use std::f32::consts::{FRAC_PI_2, PI, TAU};
use std::sync::{Arc, Mutex, OnceLock};

use egui::{Color32, Id, Painter, Pos2, Rect, Shape, Stroke, Vec2};
use tg_emoji::EmojiTextureCache;
use tg_ui::icons::paint_style_icon;
use tg_ui::tg_style::{Icon, st};
use tg_ui::tg_theme::{EmbeddedType, Palette as ThemePalette};
use tg_ui::theme::ThemeManager;

static CALLS_DAY_PALETTE: OnceLock<ThemePalette> = OnceLock::new();
static CALLS_NIGHT_PALETTE: OnceLock<ThemePalette> = OnceLock::new();

fn calls_theme_palette(dark: bool) -> &'static ThemePalette {
    if dark {
        CALLS_NIGHT_PALETTE.get_or_init(|| {
            ThemeManager::with_embedded(EmbeddedType::Night)
                .palette()
                .clone()
        })
    } else {
        CALLS_DAY_PALETTE.get_or_init(|| {
            ThemeManager::with_embedded(EmbeddedType::Default)
                .palette()
                .clone()
        })
    }
}

/// Default 1:1 call window width (`st::callWidth`).
pub const CALL_WINDOW_WIDTH: f32 = st::call_width.0 as f32;
/// Default 1:1 call window height (`st::callHeight`).
pub const CALL_WINDOW_HEIGHT: f32 = st::call_height.0 as f32;
/// Minimum 1:1 call window width (`st::callWidthMin`).
pub const CALL_WINDOW_MIN_WIDTH: f32 = st::call_width_min.0 as f32;
/// Minimum 1:1 call window height (`st::callHeightMin`).
pub const CALL_WINDOW_MIN_HEIGHT: f32 = st::call_height_min.0 as f32;
/// Bottom controls area height (`st::callBottomControlsHeight`).
pub const CALL_BOTTOM_CONTROLS_HEIGHT: f32 = st::call_bottom_controls_height.0 as f32;
/// Call button outer width (`st::callButton.width`).
pub const CALL_BUTTON_WIDTH: f32 = st::call_button.width.0 as f32;
/// Call button outer height (`st::callButton.height`).
pub const CALL_BUTTON_HEIGHT: f32 = st::call_button.height.0 as f32;
/// Call button circular background diameter (`st::callAnswer.bgSize`).
pub const CALL_BUTTON_BG_SIZE: f32 = st::call_answer.bg_size.0 as f32;
/// Call button outer pulsing radius (`st::callAnswer.outerRadius`).
pub const CALL_BUTTON_OUTER_RADIUS: f32 = st::call_answer.outer_radius.0 as f32;
/// Call panel button transition duration in ms (`st::callPanelDuration`).
pub const CALL_PANEL_ANIM_DURATION_MS: u64 = st::call_panel_duration as u64;

/// Active call top bar height (`st::callBarHeight`).
pub const CALL_BAR_HEIGHT: f32 = st::call_bar_height.0 as f32;

/// Group call default narrow window width (`st::groupCallWidth`).
pub const GROUP_CALL_WIDTH: f32 = st::group_call_width.0 as f32;
/// Group call default narrow window height (`st::groupCallHeight`).
pub const GROUP_CALL_HEIGHT: f32 = st::group_call_height.0 as f32;
/// Group call wide mode minimum width (`st::groupCallWideModeWidthMin`).
pub const GROUP_CALL_WIDE_MIN_WIDTH: f32 = st::group_call_wide_mode_width_min.0 as f32;
/// Group call wide mode default size (`st::groupCallWideModeSize`).
pub const GROUP_CALL_WIDE_SIZE: Vec2 = Vec2::new(
    st::group_call_wide_mode_size.width.0 as f32,
    st::group_call_wide_mode_size.height.0 as f32,
);
/// Group call narrow members column width (`st::groupCallNarrowMembersWidth`).
pub const GROUP_CALL_NARROW_MEMBERS_WIDTH: f32 = st::group_call_narrow_members_width.0 as f32;
/// Group call narrow column tile height (`st::groupCallNarrowVideoHeight`).
pub const GROUP_CALL_NARROW_VIDEO_HEIGHT: f32 = st::group_call_narrow_video_height.0 as f32;
/// Group call video tile gap in wide mode (`st::groupCallVideoLargeSkip`).
pub const GROUP_CALL_VIDEO_LARGE_SKIP: i32 = st::group_call_video_large_skip.0;
/// Group call video tile gap in narrow/column mode (`st::groupCallVideoSmallSkip`).
pub const GROUP_CALL_VIDEO_SMALL_SKIP: i32 = st::group_call_video_small_skip.0;
/// Group call members margin horizontal (`st::groupCallMembersMargin.left`).
pub const GROUP_CALL_MEMBERS_MARGIN_X: i32 = st::group_call_members_margin.left.0;

/// Call-specific palette entries from `resources/colors.palette`.
#[derive(Debug, Clone, Copy)]
pub struct CallPalette {
    /// 1:1 call window background (`callBgOpaque`).
    pub call_bg: Color32,
    /// Semi-transparent control button pill background (`callBgButton`).
    pub call_bg_button: Color32,
    /// Primary call name text (`callNameFg`).
    pub call_name_fg: Color32,
    /// Secondary call status text (`callStatusFg`).
    pub call_status_fg: Color32,
    /// Mute / camera / screen share inactive button background (`callIconBg`).
    pub call_icon_bg: Color32,
    /// Icon tint on dark/colored call buttons (`callIconFg`).
    pub call_icon_fg: Color32,
    /// Active/muted white circular button background (`callIconBgActive`).
    pub call_icon_bg_active: Color32,
    /// Icon tint on white circular buttons (`callIconFgActive`).
    pub call_icon_fg_active: Color32,
    /// Answer / Start call green button (`callAnswerBg`).
    pub call_answer_bg: Color32,
    /// Answer button outer ripple ring (`callAnswerBgOuter`).
    pub call_answer_outer: Color32,
    /// Hangup / Decline red button (`callHangupBg`).
    pub call_hangup_bg: Color32,
    /// Group call window background (`groupCallBg`).
    pub group_bg: Color32,
    /// Group call active blue accent (`groupCallActiveFg`).
    pub group_active_fg: Color32,
    /// Group call members card/column background (`groupCallMembersBg`).
    pub group_members_bg: Color32,
    /// Group call member row hover background (`groupCallMembersBgOver`).
    pub group_members_bg_over: Color32,
    /// Group call member row primary text (`groupCallMembersFg`).
    pub group_members_fg: Color32,
    /// Active speaking member icon/status (`groupCallMemberActiveStatus`).
    pub group_member_active: Color32,
    /// Inactive listening member icon (`groupCallMemberInactiveIcon`).
    pub group_member_inactive_icon: Color32,
    /// Listening / raised-hand blue status (`groupCallMemberInactiveStatus`).
    pub group_member_inactive_status: Color32,
    /// Muted-by-admin red icon (`groupCallMemberMutedIcon`).
    pub group_member_muted_icon: Color32,
    /// Not-joined / subtitle muted gray (`groupCallMemberNotJoinedStatus`).
    pub group_member_sub_fg: Color32,
    /// Group call live speaking gradient start (`groupCallLive1`).
    pub group_live_1: Color32,
    /// Group call live speaking gradient end (`groupCallLive2`).
    pub group_live_2: Color32,
    /// Group call muted gradient start (`groupCallMuted1`).
    pub group_muted_1: Color32,
    /// Group call muted gradient end (`groupCallMuted2`).
    pub group_muted_2: Color32,
    /// Group call force-muted gradient stop 1 (`groupCallForceMutedBar1`).
    pub group_force_muted_1: Color32,
    /// Group call force-muted gradient stop 2 (`groupCallForceMutedBar2`).
    pub group_force_muted_2: Color32,
    /// Group call force-muted gradient stop 3 (`groupCallForceMutedBar3`).
    pub group_force_muted_3: Color32,
    /// Group call popup menu background (`groupCallMenuBg`).
    pub group_menu_bg: Color32,
    /// Group call leave button translucent red (`groupCallLeaveBg`).
    pub group_leave_bg: Color32,
    /// Text overlay on video tiles (`groupCallVideoTextFg`).
    pub group_video_text_fg: Color32,
    /// Active 1:1 top bar green/accent (`callBarBg` — classic Telegram active call green).
    pub call_bar_active_bg: Color32,
    /// Muted 1:1 top bar gray (`callBarBgMuted`).
    pub call_bar_muted_bg: Color32,
    /// Top bar foreground text/icons (`callBarFg`).
    pub call_bar_fg: Color32,
    /// Received/outgoing call arrow green (`callArrowFg`).
    pub call_arrow_fg: Color32,
    /// Missed call arrow red (`callArrowMissedFg`).
    pub call_arrow_missed_fg: Color32,
}

impl Default for CallPalette {
    fn default() -> Self {
        Self::tdesktop()
    }
}

impl CallPalette {
    /// Resolves call palette tokens from `ThemeManager`'s embedded Day or Night theme.
    #[must_use]
    pub fn from_theme(dark: bool) -> Self {
        let tp = calls_theme_palette(dark);
        let get = |name: &str, fallback: Color32| -> Color32 {
            tp.color_by_name(name).map_or(fallback, |c| {
                Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
            })
        };
        Self {
            call_bg: get("callBgOpaque", Color32::from_rgb(0x1B, 0x1F, 0x23)),
            call_bg_button: get(
                "callBgButton",
                Color32::from_rgba_unmultiplied(0x1B, 0x1F, 0x23, 0x99),
            ),
            call_name_fg: get("callNameFg", Color32::WHITE),
            call_status_fg: get("callStatusFg", Color32::from_rgb(0xAA, 0xAB, 0xAC)),
            call_icon_bg: get(
                "callIconBg",
                Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 0x28),
            ),
            call_icon_fg: get("callIconFg", Color32::WHITE),
            call_icon_bg_active: get(
                "callIconBgActive",
                Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 0xE5),
            ),
            call_icon_fg_active: get("callIconFgActive", Color32::from_rgb(0x22, 0x22, 0x22)),
            call_answer_bg: get("callAnswerBg", Color32::from_rgb(0x66, 0xC9, 0x5B)),
            call_answer_outer: get(
                "callAnswerBgOuter",
                Color32::from_rgba_unmultiplied(0x50, 0xEB, 0x41, 0x38),
            ),
            call_hangup_bg: get("callHangupBg", Color32::from_rgb(0xD7, 0x5A, 0x5A)),
            group_bg: get("groupCallBg", Color32::from_rgb(0x1A, 0x20, 0x26)),
            group_active_fg: get("groupCallActiveFg", Color32::from_rgb(0x4D, 0xB8, 0xFF)),
            group_members_bg: get("groupCallMembersBg", Color32::from_rgb(0x2C, 0x33, 0x3D)),
            group_members_bg_over: get(
                "groupCallMembersBgOver",
                Color32::from_rgb(0x32, 0x3A, 0x45),
            ),
            group_members_fg: get("groupCallMembersFg", Color32::WHITE),
            group_member_active: get(
                "groupCallMemberActiveStatus",
                Color32::from_rgb(0x8D, 0xEB, 0x90),
            ),
            group_member_inactive_icon: get(
                "groupCallMemberInactiveIcon",
                Color32::from_rgb(0x84, 0x88, 0x8F),
            ),
            group_member_inactive_status: get(
                "groupCallMemberInactiveStatus",
                Color32::from_rgb(0x61, 0xC0, 0xFF),
            ),
            group_member_muted_icon: get(
                "groupCallMemberMutedIcon",
                Color32::from_rgb(0xED, 0x73, 0x72),
            ),
            group_member_sub_fg: get(
                "groupCallMemberNotJoinedStatus",
                Color32::from_rgb(0x91, 0x97, 0x9E),
            ),
            group_live_1: get("groupCallLive1", Color32::from_rgb(0x0D, 0xCC, 0x39)),
            group_live_2: get("groupCallLive2", Color32::from_rgb(0x0B, 0xB6, 0xBD)),
            group_muted_1: get("groupCallMuted1", Color32::from_rgb(0x09, 0x92, 0xEF)),
            group_muted_2: get("groupCallMuted2", Color32::from_rgb(0x16, 0xCC, 0xFB)),
            group_force_muted_1: get(
                "groupCallForceMutedBar1",
                Color32::from_rgb(0xC6, 0x54, 0x93),
            ),
            group_force_muted_2: get(
                "groupCallForceMutedBar2",
                Color32::from_rgb(0x7A, 0x6A, 0xF1),
            ),
            group_force_muted_3: get(
                "groupCallForceMutedBar3",
                Color32::from_rgb(0x5F, 0x95, 0xE8),
            ),
            group_menu_bg: get("groupCallMenuBg", Color32::from_rgb(0x29, 0x2D, 0x33)),
            group_leave_bg: get(
                "groupCallLeaveBg",
                Color32::from_rgba_unmultiplied(0xF7, 0x5C, 0x5C, 0xB0),
            ),
            group_video_text_fg: get(
                "groupCallVideoTextFg",
                Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 0xE8),
            ),
            call_bar_active_bg: get("callBarBg", Color32::from_rgb(0x2C, 0xA8, 0x5A)),
            call_bar_muted_bg: get("callBarBgMuted", Color32::from_rgb(0x73, 0x7A, 0x82)),
            call_bar_fg: get("callBarFg", Color32::WHITE),
            call_arrow_fg: get("callArrowFg", Color32::from_rgb(0x2D, 0xAD, 0x2D)),
            call_arrow_missed_fg: get("callArrowMissedFg", Color32::from_rgb(0xDD, 0x5B, 0x4A)),
        }
    }

    /// Exact color table resolved from `ThemeManager`'s embedded default theme (`resources/colors.palette`).
    #[must_use]
    pub fn tdesktop() -> Self {
        Self::from_theme(false)
    }
}

/// Linearly interpolates two [`Color32`] values in unmultiplied sRGBA space (`t` clamped to `0.0..=1.0`).
#[must_use]
pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let [ar, ag, ab, aa] = a.to_srgba_unmultiplied();
    let [br, bg, bb, ba] = b.to_srgba_unmultiplied();
    let mix =
        |x: u8, y: u8| -> u8 { (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8 };
    Color32::from_rgba_unmultiplied(mix(ar, br), mix(ag, bg), mix(ab, bb), mix(aa, ba))
}

/// Scales the alpha channel of `color` by `factor` (`0.0..=1.0`) in unmultiplied sRGBA space.
#[must_use]
pub fn with_alpha(color: Color32, factor: f32) -> Color32 {
    let [r, g, b, alpha] = color.to_srgba_unmultiplied();
    let a = (f32::from(alpha) * factor.clamp(0.0, 1.0)).round() as u8;
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

/// Paints a smooth horizontal two-color gradient filling `rect`.
pub fn paint_horizontal_gradient(painter: &Painter, rect: Rect, left: Color32, right: Color32) {
    let mut mesh = egui::Mesh::default();
    let idx = mesh.vertices.len() as u32;
    mesh.colored_vertex(rect.left_top(), left);
    mesh.colored_vertex(rect.right_top(), right);
    mesh.colored_vertex(rect.right_bottom(), right);
    mesh.colored_vertex(rect.left_bottom(), left);
    mesh.add_triangle(idx, idx + 1, idx + 2);
    mesh.add_triangle(idx, idx + 2, idx + 3);
    painter.add(Shape::mesh(mesh));
}

/// Paints a smooth radial background gradient centered at `center` using a vertex-interpolated
/// polar mesh (`calls_panel_background.cpp`), eliminating concentric banding.
pub fn paint_radial_backdrop(
    painter: &Painter,
    rect: Rect,
    center: Pos2,
    inner: Color32,
    outer: Color32,
) {
    painter.rect_filled(rect, 0.0, outer);
    let max_radius = rect
        .min
        .distance(center)
        .max(rect.max.distance(center))
        .max(Pos2::new(rect.right(), rect.top()).distance(center))
        .max(Pos2::new(rect.left(), rect.bottom()).distance(center))
        .max(1.0);

    const SECTORS: usize = 48;
    const RINGS: usize = 16;
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(center, inner);

    for ring in 1..=RINGS {
        let frac = ring as f32 / RINGS as f32;
        let r = max_radius * frac;
        // Smooth ease-out curve for rich glow around the userpic.
        let t = (frac * frac * (3.0 - 2.0 * frac)).clamp(0.0, 1.0);
        let col = lerp_color(inner, outer, t);
        for s in 0..SECTORS {
            let theta = (s as f32 / SECTORS as f32) * TAU;
            mesh.colored_vertex(
                Pos2::new(center.x + theta.cos() * r, center.y + theta.sin() * r),
                col,
            );
        }
    }

    // Center fan.
    for s in 0..SECTORS as u32 {
        let next = (s + 1) % (SECTORS as u32);
        mesh.add_triangle(0, 1 + s, 1 + next);
    }
    // Outer concentric quads.
    for ring in 1..RINGS as u32 {
        let inner_base = 1 + (ring - 1) * (SECTORS as u32);
        let outer_base = 1 + ring * (SECTORS as u32);
        for s in 0..SECTORS as u32 {
            let next = (s + 1) % (SECTORS as u32);
            mesh.add_triangle(inner_base + s, outer_base + s, outer_base + next);
            mesh.add_triangle(inner_base + s, outer_base + next, inner_base + next);
        }
    }
    painter.add(Shape::mesh(mesh));
}

/// Paints a circular avatar with initials and subtle top highlight.
pub fn paint_avatar(
    painter: &Painter,
    center: Pos2,
    radius: f32,
    bg: Color32,
    initials: &str,
    font_size: f32,
) {
    painter.circle_filled(center, radius, bg);
    painter.circle_stroke(
        center,
        radius,
        Stroke::new(1.5, Color32::from_rgba_unmultiplied(255, 255, 255, 36)),
    );
    painter.text(
        center,
        egui::Align2::CENTER_CENTER,
        initials,
        egui::FontId::proportional(font_size),
        Color32::WHITE,
    );
}

fn paint_tinted_style_icon(
    painter: &Painter,
    center: Pos2,
    size: f32,
    icon: Icon,
    color: Color32,
) -> bool {
    let dark = painter.ctx().global_style().visuals.dark_mode;
    let rect = Rect::from_center_size(center, Vec2::splat(size));
    paint_style_icon(painter, rect, icon, calls_theme_palette(dark), Some(color))
}

/// Paints a back arrow icon (`st::back_button_icon`).
pub fn paint_back_icon(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    if paint_tinted_style_icon(painter, center, size, st::back_button_icon, color) {
        return;
    }
    let stroke = Stroke::new((size * 0.14).max(1.4), color);
    painter.add(Shape::line(
        vec![
            Pos2::new(center.x + size * 0.18, center.y - size * 0.28),
            Pos2::new(center.x - size * 0.16, center.y),
            Pos2::new(center.x + size * 0.18, center.y + size * 0.28),
        ],
        stroke,
    ));
}

/// Paints an invited/calling status icon (`st::group_call_narrow_calling_icon` / `st::group_call_narrow_invited_icon`).
pub fn paint_invited_or_calling_icon(
    painter: &Painter,
    center: Pos2,
    size: f32,
    color: Color32,
    calling: bool,
) {
    let icon = if calling {
        st::group_call_narrow_calling_icon
    } else {
        st::group_call_narrow_invited_icon
    };
    if paint_tinted_style_icon(painter, center, size, icon, color) {
        return;
    }
    for dx in [-4.5_f32, 0.0, 4.5] {
        painter.circle_filled(Pos2::new(center.x + dx, center.y), 1.6, color);
    }
}

fn context_emoji_cache(ctx: &egui::Context) -> Arc<Mutex<EmojiTextureCache>> {
    let key = Id::unique("rustgram_calls_emoji_texture_cache");
    ctx.data_mut(|d| {
        if let Some(existing) = d.get_temp::<Arc<Mutex<EmojiTextureCache>>>(key) {
            existing
        } else {
            let cache = Arc::new(Mutex::new(EmojiTextureCache::new()));
            d.insert_temp(key, Arc::clone(&cache));
            cache
        }
    })
}

/// Paints a rotated phone handset icon (`angle_deg = 0.0` is Answer upright, `135.0` is Hangup).
///
/// Uses tdesktop's `calls/call_answer` and `calls/call_discard` icons from `calls.style` at rest,
/// falling back to a smoothly rotated vector handset during intermediate transition frames.
pub fn paint_phone_icon(
    painter: &Painter,
    center: Pos2,
    size: f32,
    angle_deg: f32,
    color: Color32,
) {
    if angle_deg.abs() < 1.0
        && paint_tinted_style_icon(
            painter,
            center,
            size * 1.35,
            st::call_answer.button.icon,
            color,
        )
    {
        return;
    }
    if (angle_deg - 135.0).abs() < 1.0
        && paint_tinted_style_icon(
            painter,
            center,
            size * 1.35,
            st::call_hangup.button.icon,
            color,
        )
    {
        return;
    }

    let rad = angle_deg.to_radians();
    let (sin_a, cos_a) = rad.sin_cos();
    let rot = |dx: f32, dy: f32| -> Pos2 {
        Pos2::new(
            center.x + (dx * cos_a - dy * sin_a) * size,
            center.y + (dx * sin_a + dy * cos_a) * size,
        )
    };

    // Draw curved handset arc using thick segments + end caps during intermediate angle animation.
    let mut pts = Vec::with_capacity(11);
    for i in 0..=10 {
        let t = i as f32 / 10.0;
        let theta = (-0.82 * PI) + t * (0.64 * PI);
        let dx = theta.cos() * 0.42;
        let dy = theta.sin() * 0.42 + 0.14;
        pts.push(rot(dx, dy));
    }
    painter.add(Shape::line(
        pts.clone(),
        Stroke::new((size * 0.19).max(2.0), color),
    ));
    if let (Some(&first), Some(&last)) = (pts.first(), pts.last()) {
        let cap_r = (size * 0.12).max(2.0);
        painter.circle_filled(first, cap_r, color);
        painter.circle_filled(last, cap_r, color);
    }
}

/// Paints a microphone icon with an optional animated diagonal cross-out line (`cross_progress` in `0.0..=1.0`).
///
/// Uses tdesktop's `calls/call_record_active` and `calls/call_record_muted` icons from `calls.style`.
pub fn paint_mic_icon(
    painter: &Painter,
    center: Pos2,
    size: f32,
    color: Color32,
    cross_progress: f32,
    cross_color: Color32,
) {
    let p = cross_progress.clamp(0.0, 1.0);
    if p <= 0.01
        && paint_tinted_style_icon(
            painter,
            center,
            size * 1.35,
            st::call_microphone_mute.button.icon,
            color,
        )
    {
        return;
    }
    if p >= 0.99
        && paint_tinted_style_icon(
            painter,
            center,
            size * 1.35,
            st::call_microphone_unmute.button.icon,
            color,
        )
    {
        return;
    }

    if !paint_tinted_style_icon(
        painter,
        center,
        size * 1.35,
        st::call_microphone_mute.button.icon,
        color,
    ) {
        let w = size * 0.28;
        let h = size * 0.46;
        let capsule =
            Rect::from_center_size(Pos2::new(center.x, center.y - size * 0.08), Vec2::new(w, h));
        painter.rect_filled(capsule, w * 0.5, color);

        let arc_r = size * 0.27;
        let arc_center = Pos2::new(center.x, center.y - size * 0.02);
        let mut arc = Vec::with_capacity(11);
        for i in 0..=10 {
            let theta = (i as f32 / 10.0) * PI;
            arc.push(Pos2::new(
                arc_center.x + theta.cos() * arc_r,
                arc_center.y + theta.sin() * arc_r,
            ));
        }
        let stroke_w = (size * 0.09).max(1.5);
        painter.add(Shape::line(arc, Stroke::new(stroke_w, color)));
        let stem_top = Pos2::new(center.x, arc_center.y + arc_r);
        let stem_bot = Pos2::new(center.x, center.y + size * 0.38);
        painter.line_segment([stem_top, stem_bot], Stroke::new(stroke_w, color));
        painter.line_segment(
            [
                Pos2::new(center.x - size * 0.16, stem_bot.y),
                Pos2::new(center.x + size * 0.16, stem_bot.y),
            ],
            Stroke::new(stroke_w, color),
        );
    }

    // Animated diagonal mute slash (`CrossLineAnimation`).
    if p > 0.01 {
        let start = Pos2::new(center.x - size * 0.34, center.y - size * 0.34);
        let end = Pos2::new(start.x + size * 0.68 * p, start.y + size * 0.68 * p);
        painter.line_segment(
            [start, end],
            Stroke::new((size * 0.11).max(2.0), cross_color),
        );
    }
}

/// Paints a video camera icon with optional cross-out line (`calls/call_camera_active` / `call_camera_muted`).
pub fn paint_camera_icon(painter: &Painter, center: Pos2, size: f32, color: Color32, muted: bool) {
    let icon = if muted {
        st::call_camera_unmute.button.icon
    } else {
        st::call_camera_mute.button.icon
    };
    if paint_tinted_style_icon(painter, center, size * 1.35, icon, color) {
        return;
    }

    let body = Rect::from_center_size(
        Pos2::new(center.x - size * 0.1, center.y),
        Vec2::new(size * 0.54, size * 0.38),
    );
    painter.rect_filled(body, size * 0.08, color);

    let lens = vec![
        Pos2::new(body.right() + size * 0.02, center.y),
        Pos2::new(center.x + size * 0.38, center.y - size * 0.17),
        Pos2::new(center.x + size * 0.38, center.y + size * 0.17),
    ];
    painter.add(Shape::convex_polygon(lens, color, Stroke::NONE));

    if muted {
        let start = Pos2::new(center.x - size * 0.34, center.y - size * 0.30);
        let end = Pos2::new(center.x + size * 0.34, center.y + size * 0.30);
        painter.line_segment([start, end], Stroke::new((size * 0.10).max(2.0), color));
    }
}

/// Paints a screen share icon (`calls/calls_present` from `calls.style`).
pub fn paint_screen_share_icon(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    if paint_tinted_style_icon(
        painter,
        center,
        size * 1.35,
        st::call_screencast_on.button.icon,
        color,
    ) {
        return;
    }

    let stroke_w = (size * 0.09).max(1.5);
    let monitor = Rect::from_center_size(
        Pos2::new(center.x, center.y - size * 0.06),
        Vec2::new(size * 0.68, size * 0.46),
    );
    painter.rect_stroke(
        monitor,
        size * 0.08,
        Stroke::new(stroke_w, color),
        egui::StrokeKind::Middle,
    );
    painter.line_segment(
        [
            Pos2::new(center.x - size * 0.18, monitor.bottom() + size * 0.12),
            Pos2::new(center.x + size * 0.18, monitor.bottom() + size * 0.12),
        ],
        Stroke::new(stroke_w, color),
    );
}

/// Paints a settings gear icon (`calls/calls_settings` from `calls.style`).
pub fn paint_gear_icon(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    if paint_tinted_style_icon(
        painter,
        center,
        size * 1.35,
        st::group_call_settings.button.icon,
        color,
    ) {
        return;
    }

    let stroke_w = (size * 0.11).max(1.8);
    let r = size * 0.24;
    painter.circle_stroke(center, r, Stroke::new(stroke_w, color));
    for i in 0..8 {
        let a = (i as f32 / 8.0) * TAU;
        let (s, c) = a.sin_cos();
        let p0 = Pos2::new(center.x + c * (r + 1.0), center.y + s * (r + 1.0));
        let p1 = Pos2::new(
            center.x + c * (r + size * 0.14),
            center.y + s * (r + size * 0.14),
        );
        painter.line_segment([p0, p1], Stroke::new(stroke_w, color));
    }
}

/// Paints a 5-point star (filled or outline) for the post-call rating widget (`calls/call_rating*`).
pub fn paint_star(painter: &Painter, center: Pos2, outer_r: f32, color: Color32, filled: bool) {
    let icon = if filled {
        st::call_rating_star_filled
    } else {
        st::call_rating_star.icon
    };
    if paint_tinted_style_icon(painter, center, outer_r * 2.2, icon, color) {
        return;
    }

    let inner_r = outer_r * 0.44;
    let mut pts = Vec::with_capacity(10);
    for i in 0..10 {
        let angle = -FRAC_PI_2 + (i as f32) * (PI / 5.0);
        let r = if i % 2 == 0 { outer_r } else { inner_r };
        pts.push(Pos2::new(
            center.x + angle.cos() * r,
            center.y + angle.sin() * r,
        ));
    }
    if filled {
        let mut mesh = egui::Mesh::default();
        let c_idx = mesh.vertices.len() as u32;
        mesh.colored_vertex(center, color);
        for &p in &pts {
            mesh.colored_vertex(p, color);
        }
        for i in 0..pts.len() as u32 {
            let next = (i + 1) % (pts.len() as u32);
            mesh.add_triangle(c_idx, c_idx + 1 + i, c_idx + 1 + next);
        }
        painter.add(Shape::mesh(mesh));
    } else {
        painter.add(Shape::closed_line(pts, Stroke::new(2.0, color)));
    }
}

/// Paints a directional call arrow (`calls/call_arrow_out` / `calls/call_arrow_in`).
pub fn paint_call_arrow(painter: &Painter, rect: Rect, outgoing: bool, color: Color32) {
    let icon = if outgoing {
        st::call_arrow_out
    } else {
        st::call_arrow_in
    };
    let dark = painter.ctx().global_style().visuals.dark_mode;
    if paint_style_icon(painter, rect, icon, calls_theme_palette(dark), Some(color)) {
        return;
    }

    let stroke = Stroke::new(1.8, color);
    if outgoing {
        let from = Pos2::new(rect.left() + 2.0, rect.bottom() - 2.0);
        let to = Pos2::new(rect.right() - 2.0, rect.top() + 2.0);
        painter.line_segment([from, to], stroke);
        painter.line_segment([to, Pos2::new(to.x - 5.0, to.y)], stroke);
        painter.line_segment([to, Pos2::new(to.x, to.y + 5.0)], stroke);
    } else {
        let from = Pos2::new(rect.right() - 2.0, rect.top() + 2.0);
        let to = Pos2::new(rect.left() + 2.0, rect.bottom() - 2.0);
        painter.line_segment([from, to], stroke);
        painter.line_segment([to, Pos2::new(to.x + 5.0, to.y)], stroke);
        painter.line_segment([to, Pos2::new(to.x, to.y - 5.0)], stroke);
    }
}

/// Paints a checkmark icon (`default_checkbox_check` via `st::default_checkbox_icon`).
pub fn paint_check_icon(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    if paint_tinted_style_icon(painter, center, size, st::default_checkbox_icon, color) {
        return;
    }
    let stroke = Stroke::new((size * 0.16).max(1.6), color);
    let p0 = Pos2::new(center.x - size * 0.28, center.y + size * 0.02);
    let p1 = Pos2::new(center.x - size * 0.06, center.y + size * 0.24);
    let p2 = Pos2::new(center.x + size * 0.30, center.y - size * 0.22);
    painter.add(Shape::line(vec![p0, p1, p2], stroke));
}

/// Paints a raised-hand icon (`calls/group_calls_raised_hand` from `calls.style`).
pub fn paint_hand_icon(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    if paint_tinted_style_icon(
        painter,
        center,
        size * 1.25,
        st::group_call_member_raised_hand,
        color,
    ) {
        return;
    }
    let stroke = Stroke::new((size * 0.12).max(1.5), color);
    let palm = Rect::from_center_size(
        Pos2::new(center.x, center.y + size * 0.12),
        Vec2::new(size * 0.42, size * 0.36),
    );
    painter.rect_filled(palm, size * 0.10, color);
    for dx in [-0.15_f32, -0.05, 0.05, 0.15] {
        painter.line_segment(
            [
                Pos2::new(center.x + dx * size, center.y - size * 0.04),
                Pos2::new(center.x + dx * size, center.y - size * 0.32),
            ],
            stroke,
        );
    }
}

/// Paints a small downward chevron (`calendar_down` via `st::group_call_calendar_next_disabled`).
pub fn paint_chevron_down_icon(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    if paint_tinted_style_icon(
        painter,
        center,
        size * 1.15,
        st::group_call_calendar_next_disabled,
        color,
    ) {
        return;
    }
    let stroke = Stroke::new((size * 0.16).max(1.4), color);
    let pts = vec![
        Pos2::new(center.x - size * 0.28, center.y - size * 0.12),
        Pos2::new(center.x, center.y + size * 0.16),
        Pos2::new(center.x + size * 0.28, center.y - size * 0.12),
    ];
    painter.add(Shape::line(pts, stroke));
}

/// Paints a close/cancel `X` icon (`calls/call_cancel` or `title_button_close`).
pub fn paint_close_icon(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let icon = if size >= 18.0 {
        st::call_cancel.button.icon
    } else {
        st::group_call_title_close_icon
    };
    if paint_tinted_style_icon(painter, center, size * 1.15, icon, color) {
        return;
    }
    let r = size * 0.30;
    let stroke = Stroke::new((size * 0.13).max(1.5), color);
    painter.line_segment(
        [
            Pos2::new(center.x - r, center.y - r),
            Pos2::new(center.x + r, center.y + r),
        ],
        stroke,
    );
    painter.line_segment(
        [
            Pos2::new(center.x + r, center.y - r),
            Pos2::new(center.x - r, center.y + r),
        ],
        stroke,
    );
}

/// Paints a window minimize icon (`title_button_minimize` via `st::group_call_title_minimize_icon`).
pub fn paint_minimize_icon(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    if paint_tinted_style_icon(
        painter,
        center,
        size * 1.15,
        st::group_call_title_minimize_icon,
        color,
    ) {
        return;
    }
    let half = size * 0.32;
    painter.line_segment(
        [
            Pos2::new(center.x - half, center.y + size * 0.08),
            Pos2::new(center.x + half, center.y + size * 0.08),
        ],
        Stroke::new((size * 0.13).max(1.5), color),
    );
}

/// Paints a pin-on-top icon (`calls/navbar_pin_filled` when pinned, `calls/navbar_pin` otherwise).
pub fn paint_pin_icon(painter: &Painter, center: Pos2, size: f32, color: Color32, pinned: bool) {
    let icon = if pinned {
        st::call_pinned_on_top_icon
    } else {
        st::call_pin_on_top.icon
    };
    if paint_tinted_style_icon(painter, center, size * 1.2, icon, color) {
        return;
    }
    let stroke = Stroke::new((size * 0.12).max(1.5), color);
    painter.circle_stroke(
        Pos2::new(center.x, center.y - size * 0.12),
        size * 0.22,
        stroke,
    );
    painter.line_segment(
        [
            Pos2::new(center.x, center.y + size * 0.10),
            Pos2::new(center.x, center.y + size * 0.36),
        ],
        stroke,
    );
}

/// Paints a sidebar/participants column toggle icon (`calls/navbar_column` from `calls.style`).
pub fn paint_sidebar_icon(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    if paint_tinted_style_icon(
        painter,
        center,
        size * 1.2,
        st::group_call_column_on_top.icon,
        color,
    ) {
        return;
    }
    let rect = Rect::from_center_size(center, Vec2::new(size * 0.80, size * 0.64));
    let stroke = Stroke::new((size * 0.10).max(1.4), color);
    painter.rect_stroke(rect, size * 0.10, stroke, egui::StrokeKind::Middle);
    let split_x = rect.right() - rect.width() * 0.34;
    painter.line_segment(
        [
            Pos2::new(split_x, rect.top()),
            Pos2::new(split_x, rect.bottom()),
        ],
        stroke,
    );
}

/// Paints one emoji of the 4-emoji call encryption fingerprint using `tg-emoji`'s sprite atlas
/// (normalizing `U+FE0F` variation selectors for BMP & keycap emoji).
pub fn paint_fingerprint_emoji_glyph(painter: &Painter, center: Pos2, radius: f32, emoji: &str) {
    let ctx = painter.ctx().clone();
    let rect = Rect::from_center_size(center, Vec2::splat((radius * 2.25).max(12.0)));
    let cache = context_emoji_cache(&ctx);
    if let Ok(mut guard) = cache.lock() {
        // Try exact sequence, then VS16-normalized forms (`calls_emoji_fingerprint` omits U+FE0F
        // on BMP symbols like ☎️/✈️/✌️ and keycaps like 1️⃣..0️⃣).
        if try_paint_emoji_sprite(&mut guard, painter, &ctx, emoji, rect) {
            return;
        }
        if emoji.contains('\u{20E3}') && !emoji.contains('\u{FE0F}') {
            let keycap = emoji.replace('\u{20E3}', "\u{FE0F}\u{20E3}");
            if try_paint_emoji_sprite(&mut guard, painter, &ctx, &keycap, rect) {
                return;
            }
        }
        if !emoji.ends_with('\u{FE0F}') {
            let with_vs16 = format!("{emoji}\u{FE0F}");
            if try_paint_emoji_sprite(&mut guard, painter, &ctx, &with_vs16, rect) {
                return;
            }
        } else {
            let stripped = emoji.trim_end_matches('\u{FE0F}');
            if try_paint_emoji_sprite(&mut guard, painter, &ctx, stripped, rect) {
                return;
            }
        }
        // Fallback if an emoji is absent from the atlas: draw a crisp badge instead of a
        // missing-glyph tofu rectangle.
        paint_emoji_fallback_badge(painter, center, radius, emoji);
    } else {
        paint_emoji_fallback_badge(painter, center, radius, emoji);
    }
}

fn paint_emoji_fallback_badge(painter: &Painter, center: Pos2, radius: f32, emoji: &str) {
    let chars: Vec<char> = emoji.chars().collect();
    // 1. Keycap digits (`'0'..='9'` + `\u{20E3}`).
    if let Some(&first) = chars.first()
        && first.is_ascii_digit()
        && chars.contains(&'\u{20E3}')
    {
        let side = (radius * 2.0).max(14.0);
        let box_rect = Rect::from_center_size(center, Vec2::splat(side));
        painter.rect_filled(box_rect, side * 0.22, Color32::from_rgb(0x3B, 0x82, 0xF6));
        painter.text(
            center,
            egui::Align2::CENTER_CENTER,
            first.to_string(),
            egui::FontId::proportional((radius * 1.45).max(11.0)),
            Color32::WHITE,
        );
        return;
    }

    // 2. Regional indicator country flags (two codepoints in `U+1F1E6..=U+1F1FF`).
    if chars.len() >= 2
        && ('\u{1F1E6}'..='\u{1F1FF}').contains(&chars[0])
        && ('\u{1F1E6}'..='\u{1F1FF}').contains(&chars[1])
    {
        let c0 = (b'A' + (chars[0] as u32 - 0x1F1E6) as u8) as char;
        let c1 = (b'A' + (chars[1] as u32 - 0x1F1E6) as u8) as char;
        let code = format!("{c0}{c1}");
        let pill = Rect::from_center_size(
            center,
            Vec2::new((radius * 2.3).max(18.0), (radius * 1.7).max(14.0)),
        );
        let (bg, fg) = match code.as_str() {
            "JP" => (
                Color32::from_rgb(0xF8, 0xFA, 0xFC),
                Color32::from_rgb(0xBC, 0x00, 0x2D),
            ),
            "DE" => (
                Color32::from_rgb(0xDD, 0x00, 0x00),
                Color32::from_rgb(0xFF, 0xCE, 0x00),
            ),
            "FR" => (Color32::from_rgb(0x00, 0x55, 0xA4), Color32::WHITE),
            "IT" => (Color32::from_rgb(0x00, 0x92, 0x46), Color32::WHITE),
            "ES" => (
                Color32::from_rgb(0xAA, 0x15, 0x1B),
                Color32::from_rgb(0xF1, 0xBF, 0x00),
            ),
            "GB" | "US" => (Color32::from_rgb(0x1D, 0x4E, 0xD8), Color32::WHITE),
            "CN" => (
                Color32::from_rgb(0xDE, 0x29, 0x10),
                Color32::from_rgb(0xFF, 0xDE, 0x00),
            ),
            "KR" => (
                Color32::from_rgb(0xF8, 0xFA, 0xFC),
                Color32::from_rgb(0x00, 0x47, 0xA0),
            ),
            _ => (Color32::from_rgb(0x47, 0x55, 0x69), Color32::WHITE),
        };
        painter.rect_filled(pill, 3.0, bg);
        painter.text(
            center,
            egui::Align2::CENTER_CENTER,
            code,
            egui::FontId::proportional((radius * 1.15).max(9.5)),
            fg,
        );
        return;
    }

    // 3. Deterministic colored symbol badge for any other unmapped glyph.
    let hash = emoji.bytes().fold(0u32, |acc, b| {
        acc.wrapping_mul(31).wrapping_add(u32::from(b))
    });
    let hue_colors = [
        Color32::from_rgb(0xEF, 0x44, 0x44),
        Color32::from_rgb(0xF5, 0x9E, 0x0B),
        Color32::from_rgb(0x10, 0xB9, 0x81),
        Color32::from_rgb(0x3B, 0x82, 0xF6),
        Color32::from_rgb(0x8B, 0x5C, 0xF6),
        Color32::from_rgb(0xEC, 0x48, 0x99),
    ];
    let bg = hue_colors[(hash as usize) % hue_colors.len()];
    painter.circle_filled(center, radius.max(7.0), bg);
    painter.circle_filled(center, (radius * 0.42).max(3.0), Color32::WHITE);
}

fn try_paint_emoji_sprite(
    cache: &mut EmojiTextureCache,
    painter: &Painter,
    ctx: &egui::Context,
    seq: &str,
    rect: Rect,
) -> bool {
    let Some((emoji_ref, matched)) = tg_text::emoji::find_emoji(seq, 0) else {
        return false;
    };
    if matched == 0 {
        return false;
    }
    cache.paint_emoji(painter, ctx, emoji_ref, rect, Color32::WHITE)
}

/// Paints the 4-bar call signal strength indicator (`calls_signal_bars.cpp`).
#[allow(clippy::too_many_arguments)]
pub fn paint_signal_bars(
    painter: &Painter,
    origin: Pos2,
    state: tg_calls::SignalBarsState,
    bar_width: f32,
    bar_skip: f32,
    min_height: f32,
    max_height: f32,
    color: Color32,
) {
    for i in 0..tg_calls::SIGNAL_BAR_COUNT {
        let h =
            tg_calls::SignalBarsState::bar_height(i, f64::from(min_height), f64::from(max_height))
                as f32;
        let x = origin.x + (i as f32) * (bar_width + bar_skip);
        let y = origin.y + (max_height - h);
        let rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(bar_width, h));
        let bar_color = if state.is_bar_active(i) {
            color
        } else {
            with_alpha(color, 0.45)
        };
        painter.rect_filled(rect, (bar_width * 0.5).max(1.0), bar_color);
    }
}

/// Paints an organic undulating audio blob circle around `center` (port of `ui/paint/blobs.cpp`).
pub fn paint_audio_blob(
    painter: &Painter,
    center: Pos2,
    base_radius: f32,
    amplitude: f32,
    phase: f32,
    segments: usize,
    color: Color32,
) {
    let segs = segments.max(6);
    let steps = segs * 4;
    let mut mesh = egui::Mesh::default();
    let c_idx = mesh.vertices.len() as u32;
    mesh.colored_vertex(center, color);
    for i in 0..steps {
        let theta = (i as f32 / steps as f32) * TAU;
        let wobble = (theta * (segs as f32 * 0.5) + phase).sin() * amplitude * 0.65
            + (theta * (segs as f32) - phase * 1.3).cos() * amplitude * 0.35;
        let r = (base_radius + wobble).max(1.0);
        mesh.colored_vertex(
            Pos2::new(center.x + theta.cos() * r, center.y + theta.sin() * r),
            color,
        );
    }
    for i in 0..steps as u32 {
        let next = (i + 1) % (steps as u32);
        mesh.add_triangle(c_idx, c_idx + 1 + i, c_idx + 1 + next);
    }
    painter.add(Shape::mesh(mesh));
}
