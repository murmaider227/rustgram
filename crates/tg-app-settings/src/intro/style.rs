//! Layout constants, colors, animations, and Rustgram icon rasterization for Intro.
//!
//! Port origin: `Telegram/SourceFiles/intro/intro.style`,
//! `Telegram/SourceFiles/intro/intro_step.cpp`,
//! `Telegram/SourceFiles/ui/effects/shake_animation.cpp`,
//! `Telegram/lib_ui/ui/basic.style`, and `resources/colors.palette`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use egui::epaint::Mesh;
use egui::{
    Align, Color32, ColorImage, CornerRadius, FontId, Frame, Pos2, Rect, Response, RichText, Sense,
    Stroke, TextEdit, TextureHandle, TextureOptions, Ui, Vec2,
};
use resvg::{tiny_skia, usvg};

use tg_app_core::stand_in::theme::Palette;

// --- Layout constants from `intro.style` and `basic.style` ---

/// Height of the top welcome cover (`st::introCoverHeight`).
pub const INTRO_COVER_HEIGHT: f32 = 208.0;
/// Maximum width of the cover artwork span (`st::introCoverMaxWidth`).
pub const INTRO_COVER_MAX_WIDTH: f32 = 880.0;
/// Step content column width (`st::introStepWidth`).
pub const INTRO_STEP_WIDTH: f32 = 380.0;
/// Standard primary action button width (`st::introNextButton.width`).
pub const INTRO_NEXT_BUTTON_WIDTH: f32 = 300.0;
/// Standard primary action button height (`st::introNextButton.height`).
pub const INTRO_NEXT_BUTTON_HEIGHT: f32 = 42.0;
/// Standard primary action button corner radius (`st::introNextButton.radius`).
pub const INTRO_NEXT_BUTTON_RADIUS: u8 = 6;
/// Step body vertical span (`st::introStepHeight`).
pub const INTRO_STEP_HEIGHT: f32 = 384.0;
/// Total centered intro height (`st::introHeight`).
pub const INTRO_HEIGHT: f32 = 406.0;
/// Minimum top offset of step content (`st::introStepTopMin`).
pub const INTRO_STEP_TOP_MIN: f32 = 76.0;
/// Vertical offset of Next button from contentTop (`st::introNextTop`).
pub const INTRO_NEXT_TOP: f32 = 266.0;
/// Step slide animation duration in milliseconds (`st::introSlideDuration`).
pub const INTRO_SLIDE_DURATION_MS: u64 = 200;
/// Cover reveal/hide animation duration in milliseconds (`st::introCoverDuration`).
pub const INTRO_COVER_DURATION_MS: u64 = 200;
/// Country selector field width (`st::introCountry.width`).
pub const INTRO_COUNTRY_WIDTH: f32 = 300.0;
/// Country calling code field width (`st::introCountryCode.width`).
pub const INTRO_COUNTRY_CODE_WIDTH: f32 = 64.0;
/// Local phone number field width (`st::introPhone.width`).
pub const INTRO_PHONE_WIDTH: f32 = 225.0;
/// Single code digit box height (`st::introCodeDigitHeight`).
pub const INTRO_CODE_DIGIT_HEIGHT: f32 = 50.0;
/// Single code digit box width (`introCodeDigitHeight * 0.8` in `intro_code_input.cpp`).
pub const INTRO_CODE_DIGIT_WIDTH: f32 = 40.0;
/// Horizontal gap between code digit boxes (`st::introCodeDigitSkip`).
pub const INTRO_CODE_DIGIT_SKIP: f32 = 10.0;
/// Maximum QR image logical size (`st::introQrMaxSize`).
pub const INTRO_QR_MAX_SIZE: u32 = 180;
/// Padding around the QR image on its white card (`st::introQrBackgroundSkip`).
pub const INTRO_QR_BG_SKIP: f32 = 12.0;
/// Corner radius of the white QR card (`st::introQrBackgroundRadius`).
pub const INTRO_QR_BG_RADIUS: u8 = 8;
/// Center logo badge diameter on the QR image (`st::introQrCenterSize`).
pub const INTRO_QR_CENTER_SIZE: u32 = 44;
/// Horizontal shake amplitude in pixels (`st::shakeShift`).
pub const SHAKE_SHIFT: f32 = 4.0;
/// Horizontal shake total duration in milliseconds (`st::shakeDuration`).
pub const SHAKE_DURATION_MS: u64 = 300;

// --- Palette constants from `resources/colors.palette` ---

/// Intro cover gradient top color (`introCoverTopBg`).
pub const INTRO_COVER_TOP_BG: Color32 = Color32::from_rgb(0x0F, 0x89, 0xD0);
/// Intro cover gradient bottom color (`introCoverBottomBg`).
pub const INTRO_COVER_BOTTOM_BG: Color32 = Color32::from_rgb(0x39, 0xB0, 0xF0);
/// Intro cover decorative silhouette color (`introCoverIconsFg`).
pub const INTRO_COVER_ICONS_FG: Color32 = Color32::from_rgb(0x5E, 0xC6, 0xFF);
/// Focused input underline color (`activeLineFg`).
pub const ACTIVE_LINE_FG: Color32 = Color32::from_rgb(0x37, 0xA1, 0xDE);
/// Error input underline / digit box border color (`activeLineFgError`).
pub const ACTIVE_LINE_FG_ERROR: Color32 = Color32::from_rgb(0xE4, 0x83, 0x83);
/// Neutral code digit border color in Day mode (`windowBgRipple`).
pub const CODE_DIGIT_BORDER_DAY: Color32 = Color32::from_rgb(0xE5, 0xE5, 0xE5);
/// Attention button hover fill (`attentionButtonBgOver`).
pub const ATTENTION_BG_OVER: Color32 = Color32::from_rgb(0xFC, 0xDF, 0xDE);

/// Embedded GPL-compatible Rustgram icon SVG (`assets/icon.svg`).
pub const TGFORK_ICON_SVG: &[u8] = include_bytes!("../../../../assets/icon.svg");

/// Computes tdesktop's 6-segment horizontal shake shift for normalized `progress` in `0.0..=1.0`.
///
/// Port of `Ui::DefaultShakeCallback` in `Telegram/SourceFiles/ui/effects/shake_animation.cpp`.
#[must_use]
pub fn shake_offset_at(progress: f32) -> f32 {
    const SHIFT_PROGRESS: f32 = 6.0;
    const SEGMENTS_COUNT: i32 = 5;

    let value = progress.clamp(0.0, 1.0);
    let full_progress = value * SHIFT_PROGRESS;
    let segment = (full_progress.floor() as i32).clamp(0, SEGMENTS_COUNT);
    let part = full_progress - segment as f32;
    let from = match segment {
        0 => 0.0,
        1 | 3 | 5 => 1.0,
        _ => -1.0,
    };
    let to = match segment {
        0 | 2 | 4 => 1.0,
        1 | 3 => -1.0,
        _ => 0.0,
    };
    let shift = from * (1.0 - part) + to * part;
    (shift * SHAKE_SHIFT).round()
}

/// Stateful horizontal error shake animation (`st::shakeDuration = 300ms`).
#[derive(Debug, Default, Clone)]
pub struct ShakeAnimation {
    started_at: Option<Instant>,
}

impl ShakeAnimation {
    /// Starts (or restarts) the 300ms shake animation from `now`.
    pub fn trigger(&mut self, now: Instant) {
        self.started_at = Some(now);
    }

    /// Stops any active shake animation immediately.
    pub fn stop(&mut self) {
        self.started_at = None;
    }

    /// Returns `true` if the animation is still active at `now`.
    #[must_use]
    pub fn is_animating(&self, now: Instant) -> bool {
        self.started_at.is_some_and(|start| {
            now.saturating_duration_since(start) < Duration::from_millis(SHAKE_DURATION_MS)
        })
    }

    /// Evaluates the current pixel shift and clears the animation once completed.
    pub fn offset(&mut self, now: Instant) -> f32 {
        let Some(start) = self.started_at else {
            return 0.0;
        };
        let elapsed = now.saturating_duration_since(start).as_secs_f32();
        let total = Duration::from_millis(SHAKE_DURATION_MS).as_secs_f32();
        if elapsed >= total {
            self.started_at = None;
            0.0
        } else {
            shake_offset_at(elapsed / total)
        }
    }
}

/// Smooth reveal/hide transition for the top welcome cover (`_coverShownAnimation` in `intro_widget.cpp`).
#[derive(Debug, Clone)]
pub struct CoverTransition {
    showing: bool,
    started_at: Instant,
    duration: Duration,
}

impl CoverTransition {
    /// Starts a cover transition (`showing = true` reveals the cover, `false` slides it out).
    #[must_use]
    pub fn start(showing: bool, now: Instant) -> Self {
        Self {
            showing,
            started_at: now,
            duration: Duration::from_millis(INTRO_COVER_DURATION_MS),
        }
    }

    /// Returns the cover visibility fraction (`0.0` hidden .. `1.0` fully visible).
    #[must_use]
    pub fn visibility(&self, now: Instant) -> f32 {
        let raw = (now.saturating_duration_since(self.started_at).as_secs_f32()
            / self.duration.as_secs_f32())
        .clamp(0.0, 1.0);
        if self.showing {
            // easeOutCirc: sqrt(1 - (t - 1)^2)
            let shifted = raw - 1.0;
            (1.0 - shifted * shifted).max(0.0).sqrt()
        } else {
            // linear hide (matching `intro_step.cpp:340`)
            1.0 - raw
        }
    }

    /// Returns `true` once the transition has finished.
    #[must_use]
    pub fn is_finished(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.started_at) >= self.duration
    }
}

/// Rasterizes `assets/icon.svg` to a square `size x size` straight-alpha RGBA buffer using `resvg`.
///
/// Never uses Telegram's paper-plane trademark; renders Rustgram's own GPL-clean icon.
#[must_use]
pub fn render_tgfork_icon_rgba(size: u32) -> Vec<u8> {
    let size = size.clamp(16, 512);
    let mut options = usvg::Options::default();
    {
        let db = options.fontdb_mut();
        db.load_system_fonts();
        for family in [
            "Segoe UI",
            "Arial",
            "Liberation Sans",
            "DejaVu Sans",
            "Noto Sans",
        ] {
            if db.faces().any(|f| {
                f.families
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case(family))
            }) {
                db.set_sans_serif_family(family);
                break;
            }
        }
    }

    if let Ok(tree) = usvg::Tree::from_data(TGFORK_ICON_SVG, &options)
        && let Some(mut pixmap) = tiny_skia::Pixmap::new(size, size)
    {
        let tree_size = tree.size();
        let sx = size as f32 / tree_size.width().max(1.0);
        let sy = size as f32 / tree_size.height().max(1.0);
        resvg::render(
            &tree,
            tiny_skia::Transform::from_scale(sx, sy),
            &mut pixmap.as_mut(),
        );

        let mut straight = Vec::with_capacity((size * size * 4) as usize);
        for px in pixmap.pixels() {
            let c = px.demultiply();
            straight.push(c.red());
            straight.push(c.green());
            straight.push(c.blue());
            straight.push(c.alpha());
        }
        return straight;
    }

    // Pure-geometric fallback if system font DB is unavailable in a minimal container.
    let mut fallback = vec![0u8; (size * size * 4) as usize];
    let r = (size as f32) * 0.22;
    for y in 0..size {
        for x in 0..size {
            let idx = ((y * size + x) * 4) as usize;
            let dx = (x as f32 - r).max(0.0).min((size as f32 - 1.0) - 2.0 * r);
            let dy = (y as f32 - r).max(0.0).min((size as f32 - 1.0) - 2.0 * r);
            let cx = x as f32 - (r + dx);
            let cy = y as f32 - (r + dy);
            if cx * cx + cy * cy <= r * r {
                fallback[idx] = 0x08;
                fallback[idx + 1] = 0x91;
                fallback[idx + 2] = 0xB2;
                fallback[idx + 3] = 0xFF;
            }
        }
    }
    fallback
}

/// Cache of `resvg`-rasterized Rustgram icons uploaded as `egui::TextureHandle`s.
#[derive(Default)]
pub struct TgforkIconCache {
    textures: HashMap<u32, TextureHandle>,
    raw_rgba: HashMap<u32, Vec<u8>>,
}

impl std::fmt::Debug for TgforkIconCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TgforkIconCache")
            .field("cached_sizes", &self.textures.len())
            .finish()
    }
}

impl TgforkIconCache {
    /// Returns cached straight-alpha RGBA bytes for `assets/icon.svg` at `size x size`.
    pub fn rgba(&mut self, size: u32) -> &[u8] {
        self.raw_rgba
            .entry(size)
            .or_insert_with(|| render_tgfork_icon_rgba(size))
    }

    /// Returns (or creates) an `egui::TextureHandle` for `assets/icon.svg` at `size x size`.
    pub fn texture(&mut self, ctx: &egui::Context, size: u32) -> TextureHandle {
        if let Some(tex) = self.textures.get(&size) {
            return tex.clone();
        }
        let rgba = self.rgba(size).to_vec();
        let img = ColorImage::from_rgba_unmultiplied([size as usize, size as usize], &rgba);
        let tex = ctx.load_texture(
            format!("rustgram_intro_icon_{size}"),
            img,
            TextureOptions::LINEAR,
        );
        self.textures.insert(size, tex.clone());
        tex
    }
}

/// Paints the top gradient welcome cover (`Step::paintCover` in `intro_step.cpp`) with the
/// Rustgram icon instead of Telegram's paper-plane trademark.
pub fn paint_intro_cover(
    ui: &mut Ui,
    screen_rect: Rect,
    cover_visibility: f32,
    icon_cache: &mut TgforkIconCache,
) {
    let vis = cover_visibility.clamp(0.0, 1.0);
    if vis <= 0.001 {
        return;
    }

    let top = -INTRO_COVER_HEIGHT * (1.0 - vis);
    let cover_height = (top + INTRO_COVER_HEIGHT).max(0.0);
    if cover_height <= 0.5 {
        return;
    }

    let cover_rect = Rect::from_min_size(
        Pos2::new(screen_rect.left(), screen_rect.top()),
        Vec2::new(screen_rect.width(), cover_height),
    );
    let painter = ui.painter_at(cover_rect);

    // Vertical linear gradient: `introCoverTopBg` (#0f89d0) -> `introCoverBottomBg` (#39b0f0).
    let mut mesh = Mesh::default();
    let v0 = mesh.vertices.len() as u32;
    mesh.colored_vertex(cover_rect.left_top(), INTRO_COVER_TOP_BG);
    mesh.colored_vertex(cover_rect.right_top(), INTRO_COVER_TOP_BG);
    mesh.colored_vertex(cover_rect.right_bottom(), INTRO_COVER_BOTTOM_BG);
    mesh.colored_vertex(cover_rect.left_bottom(), INTRO_COVER_BOTTOM_BG);
    mesh.add_triangle(v0, v0 + 1, v0 + 2);
    mesh.add_triangle(v0, v0 + 2, v0 + 3);
    painter.add(egui::Shape::mesh(mesh));

    // Geometric cloud/wave silhouettes on left & right (`introCoverIconsFg`), pure vector art.
    let cloud_color = Color32::from_rgba_unmultiplied(
        INTRO_COVER_ICONS_FG.r(),
        INTRO_COVER_ICONS_FG.g(),
        INTRO_COVER_ICONS_FG.b(),
        90,
    );
    let base_y = cover_rect.bottom();
    let span = screen_rect.width().min(INTRO_COVER_MAX_WIDTH);
    let left_anchor = screen_rect.center().x - span * 0.5;
    let right_anchor = screen_rect.center().x + span * 0.5;

    // Left cluster
    painter.circle_filled(
        Pos2::new(left_anchor + 48.0, base_y - 12.0),
        54.0,
        cloud_color,
    );
    painter.circle_filled(
        Pos2::new(left_anchor + 108.0, base_y + 4.0),
        42.0,
        cloud_color,
    );
    painter.circle_filled(
        Pos2::new(left_anchor + 158.0, base_y + 16.0),
        30.0,
        cloud_color,
    );
    // Right cluster
    painter.circle_filled(
        Pos2::new(right_anchor - 52.0, base_y - 16.0),
        58.0,
        cloud_color,
    );
    painter.circle_filled(
        Pos2::new(right_anchor - 118.0, base_y + 2.0),
        40.0,
        cloud_color,
    );
    painter.circle_filled(
        Pos2::new(right_anchor - 168.0, base_y + 18.0),
        28.0,
        cloud_color,
    );

    // Centered Rustgram icon badge (`assets/icon.svg` rasterized with `resvg`).
    let icon_size = 92.0;
    let center = Pos2::new(
        screen_rect.center().x,
        screen_rect.top() + top + INTRO_COVER_HEIGHT * 0.5,
    );
    let halo_color = Color32::from_rgba_unmultiplied(255, 255, 255, 42);
    painter.circle_filled(center, icon_size * 0.58, halo_color);

    let tex = icon_cache.texture(ui.ctx(), 184);
    let icon_rect = Rect::from_center_size(center, Vec2::splat(icon_size));
    painter.image(
        tex.id(),
        icon_rect,
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        Color32::WHITE,
    );
}

/// Primary active action button matching `st::introNextButton` (`300x42`, `r=6`).
pub fn primary_intro_button(ui: &mut Ui, width: f32, label: &str, palette: &Palette) -> Response {
    ui.add_sized(
        Vec2::new(width, INTRO_NEXT_BUTTON_HEIGHT),
        egui::Button::new(
            RichText::new(label)
                .size(14.5)
                .strong()
                .color(palette.window_fg_active),
        )
        .fill(palette.window_bg_active)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(INTRO_NEXT_BUTTON_RADIUS)),
    )
}

/// Text link button matching `st::introLink` (`windowActiveTextFg`).
pub fn link_button(ui: &mut Ui, label: &str, palette: &Palette) -> Response {
    let resp = ui.add(
        egui::Button::new(
            RichText::new(label)
                .size(13.5)
                .color(palette.window_active_text_fg),
        )
        .frame(false),
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        let underline_y = resp.rect.bottom() - 2.0;
        ui.painter().line_segment(
            [
                Pos2::new(resp.rect.left() + 2.0, underline_y),
                Pos2::new(resp.rect.right() - 2.0, underline_y),
            ],
            Stroke::new(1.0, palette.window_active_text_fg),
        );
    }
    resp
}

/// Destructive light button matching `st::introResetButton` (`attentionButtonFg`).
pub fn attention_light_button(ui: &mut Ui, label: &str, palette: &Palette) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(220.0, 36.0), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        let hover_bg = if palette.window_bg.r() > 128 {
            ATTENTION_BG_OVER
        } else {
            Color32::from_rgba_unmultiplied(0xEC, 0x5B, 0x5B, 36)
        };
        ui.painter()
            .rect_filled(rect, CornerRadius::same(4), hover_bg);
    }
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label.to_uppercase(),
        FontId::proportional(13.0),
        palette.attention_fg,
    );
    resp
}

/// Draws a tdesktop-style underline input field with optional placeholder, password mask, and error border.
#[allow(clippy::too_many_arguments)]
pub fn underline_input(
    ui: &mut Ui,
    id_salt: &str,
    value: &mut String,
    placeholder: &str,
    width: f32,
    password: bool,
    has_error: bool,
    shake_dx: f32,
    palette: &Palette,
) -> Response {
    let mut output_resp: Option<Response> = None;
    let rect_top = ui.cursor().min;
    let field_rect = Rect::from_min_size(
        Pos2::new(rect_top.x + shake_dx, rect_top.y),
        Vec2::new(width, 38.0),
    );
    ui.allocate_space(Vec2::new(width, 40.0));

    let text_rect = Rect::from_min_size(
        Pos2::new(field_rect.left() + 2.0, field_rect.top() + 6.0),
        Vec2::new((width - 4.0).max(20.0), 26.0),
    );
    ui.scope_builder(egui::UiBuilder::new().max_rect(text_rect), |ui| {
        let resp = ui.add_sized(
            text_rect.size(),
            TextEdit::singleline(value)
                .id_salt(id_salt)
                .password(password)
                .frame(Frame::NONE)
                .font(FontId::proportional(15.5))
                .text_color(palette.window_fg)
                .hint_text(
                    RichText::new(placeholder)
                        .size(15.0)
                        .color(palette.window_sub_text_fg),
                ),
        );
        output_resp = Some(resp);
    });

    let resp = output_resp.expect("underline_input always creates a TextEdit");
    let focused = resp.has_focus();
    let (line_color, line_width) = if has_error {
        (ACTIVE_LINE_FG_ERROR, 2.0)
    } else if focused {
        (ACTIVE_LINE_FG, 2.0)
    } else {
        (palette.divider, 1.0)
    };
    let y = field_rect.bottom() - 2.0;
    ui.painter().line_segment(
        [
            Pos2::new(field_rect.left(), y),
            Pos2::new(field_rect.right(), y),
        ],
        Stroke::new(line_width, line_color),
    );
    resp
}

/// Renders a phrase that may contain `**bold**` markers (as used in `resources/lang.strings`).
pub fn rich_markdown_label(
    ui: &mut Ui,
    text: &str,
    font_size: f32,
    normal_color: Color32,
    bold_color: Color32,
    centered: bool,
) {
    let mut job = egui::text::LayoutJob {
        halign: if centered { Align::Center } else { Align::LEFT },
        ..Default::default()
    };
    let normal_fmt = egui::TextFormat {
        font_id: FontId::proportional(font_size),
        color: normal_color,
        ..Default::default()
    };
    let bold_fmt = egui::TextFormat {
        font_id: FontId::proportional(font_size),
        color: bold_color,
        ..Default::default()
    };

    let mut rest = text;
    while let Some(start) = rest.find("**") {
        if start > 0 {
            job.append(&rest[..start], 0.0, normal_fmt.clone());
        }
        let after_start = &rest[start + 2..];
        if let Some(end) = after_start.find("**") {
            job.append(&after_start[..end], 0.0, bold_fmt.clone());
            rest = &after_start[end + 2..];
        } else {
            job.append(&rest[start..], 0.0, normal_fmt.clone());
            rest = "";
            break;
        }
    }
    if !rest.is_empty() {
        job.append(rest, 0.0, normal_fmt);
    }
    ui.label(job);
}
