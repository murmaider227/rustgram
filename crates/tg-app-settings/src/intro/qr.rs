//! QR code login step and tdesktop-style rounded QR renderer with centered Rustgram icon.
//!
//! Port origin: `Telegram/SourceFiles/intro/intro_qr.cpp` and
//! `Telegram/lib_qr/qr/qr_generate.cpp`.

use std::time::{Duration, Instant};

use egui::{
    Align, Color32, ColorImage, CornerRadius, Layout, Pos2, Rect, RichText, Sense, Stroke,
    StrokeKind, TextureHandle, TextureOptions, Ui, Vec2,
};
use qrcode::{Color as QrColor, EcLevel, QrCode};
use tg_api::login::QrLoginState;
use tg_lang::tr;

use crate::intro::style::{
    INTRO_QR_BG_RADIUS, INTRO_QR_BG_SKIP, INTRO_QR_CENTER_SIZE, INTRO_QR_MAX_SIZE, TgforkIconCache,
    link_button,
};
use tg_app_core::settings::backend::PasskeyHybridQrStatus;
use tg_app_core::stand_in::theme::Palette;

/// QR error-correction redundancy matching `Qr::Redundancy::Quartile` in `intro_qr.cpp:82`.
const QR_EC_LEVEL: EcLevel = EcLevel::Q;

/// Computes the cleared center module count matching `Qr::ReplaceElements` in `qr_generate.cpp:25-38`.
#[must_use]
pub const fn qr_replace_elements(size: usize) -> usize {
    let elements = size / 3;
    let shift = (size.saturating_sub(elements)) % 2;
    elements + shift
}

/// Encoded QR matrix paired with its dimension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrMatrix {
    /// Width and height in modules.
    pub size: usize,
    /// Row-major module booleans (`true` = dark).
    pub modules: Vec<bool>,
}

impl QrMatrix {
    /// Encodes `url` (e.g. `tg://login?token=...`) at Quartile error correction (`Qr::Encode`).
    #[must_use]
    pub fn encode(url: &str) -> Option<Self> {
        if url.is_empty() {
            return None;
        }
        let code = QrCode::with_error_correction_level(url.as_bytes(), QR_EC_LEVEL).ok()?;
        let size = code.width();
        let mut modules = Vec::with_capacity(size * size);
        for row in 0..size {
            for col in 0..size {
                modules.push(code[(col, row)] == QrColor::Dark);
            }
        }
        Some(Self { size, modules })
    }
}

/// Renders a QR code in Telegram Desktop's signature rounded-module style (`qr_generate.cpp`)
/// with the center area cleared and overlaid with Rustgram's `resvg`-rendered icon.
///
/// Returns `(straight_rgba, width, height)`.
#[must_use]
pub fn render_tdesktop_qr_rgba(
    url: &str,
    max_size_px: u32,
    dpr: u32,
    center_icon_rgba: Option<(&[u8], u32)>,
) -> Option<(Vec<u8>, u32, u32)> {
    let matrix = QrMatrix::encode(url)?;
    let size = matrix.size;
    if size == 0 {
        return None;
    }

    let dpr = dpr.clamp(1, 3);
    let logical_pixel = (max_size_px / size as u32).max(1);
    let pixel = (logical_pixel * dpr).max(2);
    let img_dim = size as u32 * pixel;

    let fg = [0x00u8, 0x00u8, 0x00u8, 0xFFu8];
    let bg = [0xFFu8, 0xFFu8, 0xFFu8, 0xFFu8];
    let mut canvas = vec![0xFFu8; (img_dim * img_dim * 4) as usize];

    let replace_elements = qr_replace_elements(size);
    let replace_from = (size - replace_elements) / 2;
    let replace_till = size - replace_from;

    let is_dark = |row: isize, col: isize| -> bool {
        if row < 0 || col < 0 || row >= size as isize || col >= size as isize {
            return false;
        }
        let r = row as usize;
        let c = col as usize;
        if r >= replace_from && r < replace_till && c >= replace_from && c < replace_till {
            return false;
        }
        matrix.modules[r * size + c]
    };

    let black_full = |row: isize, col: isize| -> bool {
        (is_dark(row - 1, col) && is_dark(row + 1, col))
            || (is_dark(row, col - 1) && is_dark(row, col + 1))
    };

    let white_corner = |row: isize, col: isize, dx: isize, dy: isize| -> bool {
        !is_dark(row + dy, col) || !is_dark(row, col + dx) || !is_dark(row + dy, col + dx)
    };

    let white_full = |row: isize, col: isize| -> bool {
        white_corner(row, col, -1, -1)
            && white_corner(row, col, 1, -1)
            && white_corner(row, col, 1, 1)
            && white_corner(row, col, -1, 1)
    };

    let p = pixel as i32;
    let half = p / 2;
    let skip = p - half;

    // Render all non-finder modules with tdesktop's rounded outer corners & concave inner fillets.
    for row in 0..size {
        for col in 0..size {
            if (row < 7 && (col < 7 || col >= size - 7)) || (col < 7 && row >= size - 7) {
                continue;
            }
            let x = (col as u32 * pixel) as i32;
            let y = (row as u32 * pixel) as i32;
            let r = row as isize;
            let c = col as isize;

            if is_dark(r, c) {
                if black_full(r, c) {
                    fill_rect(&mut canvas, img_dim, x, y, p, p, fg);
                } else {
                    fill_rounded_rect(&mut canvas, img_dim, x, y, p, p, p as f32 * 0.5, fg);
                    if is_dark(r - 1, c) {
                        fill_rect(&mut canvas, img_dim, x, y, p, half, fg);
                    } else if is_dark(r + 1, c) {
                        fill_rect(&mut canvas, img_dim, x, y + skip, p, half, fg);
                    }
                    if is_dark(r, c - 1) {
                        fill_rect(&mut canvas, img_dim, x, y, half, p, fg);
                    } else if is_dark(r, c + 1) {
                        fill_rect(&mut canvas, img_dim, x + skip, y, half, p, fg);
                    }
                }
            } else if !white_full(r, c) {
                // Concave inner rounding where dark modules meet orthogonally around this white cell.
                fill_rect(&mut canvas, img_dim, x, y, p, p, fg);
                fill_rounded_rect(&mut canvas, img_dim, x, y, p, p, p as f32 * 0.5, bg);
                if white_corner(r, c, -1, -1) && white_corner(r, c, 1, -1) {
                    fill_rect(&mut canvas, img_dim, x, y, p, half, bg);
                } else if white_corner(r, c, -1, 1) && white_corner(r, c, 1, 1) {
                    fill_rect(&mut canvas, img_dim, x, y + skip, p, half, bg);
                }
                if white_corner(r, c, -1, -1) && white_corner(r, c, -1, 1) {
                    fill_rect(&mut canvas, img_dim, x, y, half, p, bg);
                } else if white_corner(r, c, 1, -1) && white_corner(r, c, 1, 1) {
                    fill_rect(&mut canvas, img_dim, x + skip, y, half, p, bg);
                }
                if white_corner(r, c, -1, -1) {
                    fill_rect(&mut canvas, img_dim, x, y, half, half, bg);
                }
                if white_corner(r, c, 1, -1) {
                    fill_rect(&mut canvas, img_dim, x + skip, y, half, half, bg);
                }
                if white_corner(r, c, 1, 1) {
                    fill_rect(&mut canvas, img_dim, x + skip, y + skip, half, half, bg);
                }
                if white_corner(r, c, -1, 1) {
                    fill_rect(&mut canvas, img_dim, x, y + skip, half, half, bg);
                }
            }
        }
    }

    // Draw the 3 rounded finder patterns (`large` in `qr_generate.cpp:150-167`).
    let draw_finder = |canvas: &mut [u8], fx: i32, fy: i32| {
        fill_rounded_rect(canvas, img_dim, fx, fy, p * 7, p * 7, p as f32 * 2.0, fg);
        fill_rounded_rect(
            canvas,
            img_dim,
            fx + p,
            fy + p,
            p * 5,
            p * 5,
            p as f32 * 1.5,
            bg,
        );
        fill_rounded_rect(
            canvas,
            img_dim,
            fx + p * 2,
            fy + p * 2,
            p * 3,
            p * 3,
            p as f32,
            fg,
        );
    };
    draw_finder(&mut canvas, 0, 0);
    draw_finder(&mut canvas, ((size - 7) as u32 * pixel) as i32, 0);
    draw_finder(&mut canvas, 0, ((size - 7) as u32 * pixel) as i32);

    // Composite centered Rustgram icon badge (`introQrCenterSize = 44px` scaled by DPR).
    if let Some((icon_rgba, icon_dim)) = center_icon_rgba {
        let badge_dim = (INTRO_QR_CENTER_SIZE * dpr).min(img_dim / 2);
        let cx = img_dim as f32 * 0.5;
        let cy = img_dim as f32 * 0.5;
        let outer_r = badge_dim as f32 * 0.5 + (2 * dpr) as f32;
        fill_circle(&mut canvas, img_dim, cx, cy, outer_r, bg);
        blit_scaled_rgba(
            &mut canvas,
            img_dim,
            icon_rgba,
            icon_dim,
            ((img_dim - badge_dim) / 2) as i32,
            ((img_dim - badge_dim) / 2) as i32,
            badge_dim,
        );
    }

    Some((canvas, img_dim, img_dim))
}

fn fill_rect(canvas: &mut [u8], dim: u32, x0: i32, y0: i32, w: i32, h: i32, color: [u8; 4]) {
    let x_start = x0.max(0) as u32;
    let y_start = y0.max(0) as u32;
    let x_end = (x0 + w).min(dim as i32).max(0) as u32;
    let y_end = (y0 + h).min(dim as i32).max(0) as u32;
    for y in y_start..y_end {
        let row_off = (y * dim) as usize;
        for x in x_start..x_end {
            let idx = (row_off + x as usize) * 4;
            canvas[idx..idx + 4].copy_from_slice(&color);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_rounded_rect(
    canvas: &mut [u8],
    dim: u32,
    x0: i32,
    y0: i32,
    w: i32,
    h: i32,
    radius: f32,
    color: [u8; 4],
) {
    let r = radius.min(w as f32 * 0.5).min(h as f32 * 0.5).max(0.0);
    let x_start = x0.max(0) as u32;
    let y_start = y0.max(0) as u32;
    let x_end = (x0 + w).min(dim as i32).max(0) as u32;
    let y_end = (y0 + h).min(dim as i32).max(0) as u32;

    let left = x0 as f32 + r;
    let right = (x0 + w) as f32 - r;
    let top = y0 as f32 + r;
    let bottom = (y0 + h) as f32 - r;

    for y in y_start..y_end {
        let py = y as f32 + 0.5;
        let row_off = (y * dim) as usize;
        for x in x_start..x_end {
            let px = x as f32 + 0.5;
            let dx = if px < left {
                left - px
            } else if px > right {
                px - right
            } else {
                0.0
            };
            let dy = if py < top {
                top - py
            } else if py > bottom {
                py - bottom
            } else {
                0.0
            };
            let dist = (dx * dx + dy * dy).sqrt();
            // 1px anti-aliased edge for crisp rounded dots
            let cov = (r + 0.5 - dist).clamp(0.0, 1.0);
            if cov > 0.0 {
                let idx = (row_off + x as usize) * 4;
                blend_pixel(&mut canvas[idx..idx + 4], color, cov);
            }
        }
    }
}

fn fill_circle(canvas: &mut [u8], dim: u32, cx: f32, cy: f32, radius: f32, color: [u8; 4]) {
    let x_start = (cx - radius - 1.0).floor().max(0.0) as u32;
    let y_start = (cy - radius - 1.0).floor().max(0.0) as u32;
    let x_end = (cx + radius + 1.0).ceil().min(dim as f32) as u32;
    let y_end = (cy + radius + 1.0).ceil().min(dim as f32) as u32;
    for y in y_start..y_end {
        let dy = (y as f32 + 0.5) - cy;
        let row_off = (y * dim) as usize;
        for x in x_start..x_end {
            let dx = (x as f32 + 0.5) - cx;
            let dist = (dx * dx + dy * dy).sqrt();
            let cov = (radius + 0.5 - dist).clamp(0.0, 1.0);
            if cov > 0.0 {
                let idx = (row_off + x as usize) * 4;
                blend_pixel(&mut canvas[idx..idx + 4], color, cov);
            }
        }
    }
}

fn blit_scaled_rgba(
    canvas: &mut [u8],
    dim: u32,
    src: &[u8],
    src_dim: u32,
    dst_x: i32,
    dst_y: i32,
    dst_dim: u32,
) {
    if src_dim == 0 || dst_dim == 0 {
        return;
    }
    for y in 0..dst_dim {
        let ty = dst_y + y as i32;
        if ty < 0 || ty >= dim as i32 {
            continue;
        }
        let sy = ((y * src_dim) / dst_dim).min(src_dim - 1);
        for x in 0..dst_dim {
            let tx = dst_x + x as i32;
            if tx < 0 || tx >= dim as i32 {
                continue;
            }
            let sx = ((x * src_dim) / dst_dim).min(src_dim - 1);
            let sidx = ((sy * src_dim + sx) * 4) as usize;
            let sa = src[sidx + 3] as f32 / 255.0;
            if sa > 0.0 {
                let didx = ((ty as u32 * dim + tx as u32) * 4) as usize;
                blend_pixel(
                    &mut canvas[didx..didx + 4],
                    [src[sidx], src[sidx + 1], src[sidx + 2], 255],
                    sa,
                );
            }
        }
    }
}

fn blend_pixel(dst: &mut [u8], src: [u8; 4], alpha: f32) {
    let inv = 1.0 - alpha;
    dst[0] = (src[0] as f32 * alpha + dst[0] as f32 * inv).round() as u8;
    dst[1] = (src[1] as f32 * alpha + dst[1] as f32 * inv).round() as u8;
    dst[2] = (src[2] as f32 * alpha + dst[2] as f32 * inv).round() as u8;
    dst[3] = 255;
}

/// Actions emitted by [`QrStepState::show`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QrStepAction {
    /// Token reached its expiry (`refresh_after_secs`); request a fresh token.
    RefreshToken,
    /// User clicked `"Log in using phone number"`.
    SwitchToPhone,
    /// User clicked `"Log in using passkey"` (`QrWidget::setupPasskeyLink`).
    LoginWithPasskey,
    /// User clicked `"Sign in with passkey on another device"` (caBLE hybrid QR).
    LoginWithPasskeyHybridQr,
    /// User cancelled the active passkey hybrid QR ceremony.
    CancelPasskeyHybridQr,
}

/// UI state for `LoginStep::QrLogin` (`Intro::QrWidget`).
pub struct QrStepState {
    cached_url: String,
    texture: Option<TextureHandle>,
    cached_hybrid_url: String,
    hybrid_texture: Option<TextureHandle>,
    /// Wall-clock deadline when the current token expires and must refresh.
    pub next_refresh_at: Option<Instant>,
    /// Start instant for the loading radial spinner when no token is loaded yet.
    spinner_started: Instant,
    /// Whether the `"Log in using passkey"` link should be shown below the phone link.
    pub passkey_supported: bool,
    /// Active cross-device caBLE / hybrid passkey QR URI (`"FIDO:/..."`), if open.
    pub hybrid_qr_uri: Option<String>,
    /// Live caBLEv2 BLE proximity status for [`Self::hybrid_qr_uri`].
    pub hybrid_qr_status: PasskeyHybridQrStatus,
}

impl std::fmt::Debug for QrStepState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QrStepState")
            .field("cached_url", &self.cached_url)
            .field("has_texture", &self.texture.is_some())
            .field("hybrid_qr_uri", &self.hybrid_qr_uri)
            .field("hybrid_qr_status", &self.hybrid_qr_status)
            .field("next_refresh_at", &self.next_refresh_at)
            .finish()
    }
}

impl Default for QrStepState {
    fn default() -> Self {
        Self::new()
    }
}

impl QrStepState {
    /// Creates a new `QrStepState`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cached_url: String::new(),
            texture: None,
            cached_hybrid_url: String::new(),
            hybrid_texture: None,
            next_refresh_at: None,
            spinner_started: tg_app_core::clock::now(),
            passkey_supported: false,
            hybrid_qr_uri: None,
            hybrid_qr_status: PasskeyHybridQrStatus::Idle,
        }
    }

    /// Resets cached QR state when leaving or restarting the QR step.
    pub fn reset(&mut self) {
        self.cached_url.clear();
        self.texture = None;
        self.cached_hybrid_url.clear();
        self.hybrid_texture = None;
        self.hybrid_qr_uri = None;
        self.hybrid_qr_status = PasskeyHybridQrStatus::Idle;
        self.next_refresh_at = None;
        self.spinner_started = tg_app_core::clock::now();
    }

    /// Schedules the next token expiry refresh from `now + refresh_after_secs`.
    pub fn schedule_refresh(&mut self, refresh_after_secs: u64, now: Instant) {
        self.next_refresh_at = Some(now + Duration::from_secs(refresh_after_secs.max(1)));
    }

    /// Returns `true` if the token has expired at `now`.
    #[must_use]
    pub fn is_refresh_due(&self, now: Instant) -> bool {
        self.next_refresh_at.is_some_and(|deadline| now >= deadline)
    }

    /// Renders the QR login screen (`Intro::QrWidget`).
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut Ui,
        card_width: f32,
        qr_state: &QrLoginState,
        error_text: &str,
        now: Instant,
        icon_cache: &mut TgforkIconCache,
        palette: &Palette,
    ) -> Option<QrStepAction> {
        let mut action = None;

        // If cross-device caBLE / hybrid passkey QR mode is active, render it (`cable_box.cpp`).
        if let Some(hybrid_uri) = self.hybrid_qr_uri.clone() {
            if hybrid_uri != self.cached_hybrid_url
                && !hybrid_uri.is_empty()
                && let Some((rgba, w, h)) =
                    render_tdesktop_qr_rgba(&hybrid_uri, INTRO_QR_MAX_SIZE, 2, None)
            {
                let img = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                self.hybrid_texture = Some(ui.ctx().load_texture(
                    "intro_passkey_hybrid_qr",
                    img,
                    TextureOptions::LINEAR,
                ));
                self.cached_hybrid_url = hybrid_uri;
            }

            ui.with_layout(Layout::top_down(Align::Center), |ui| {
                let qr_logical = INTRO_QR_MAX_SIZE as f32;
                let card_side = qr_logical + 2.0 * INTRO_QR_BG_SKIP;
                let (card_rect, _) = ui.allocate_exact_size(Vec2::splat(card_side), Sense::hover());
                let painter = ui.painter();
                painter.rect_filled(
                    card_rect,
                    CornerRadius::same(INTRO_QR_BG_RADIUS),
                    Color32::WHITE,
                );
                painter.rect_stroke(
                    card_rect,
                    CornerRadius::same(INTRO_QR_BG_RADIUS),
                    Stroke::new(1.0, palette.divider),
                    StrokeKind::Outside,
                );
                if let Some(tex) = &self.hybrid_texture {
                    let qr_rect =
                        Rect::from_center_size(card_rect.center(), Vec2::splat(qr_logical));
                    painter.image(
                        tex.id(),
                        qr_rect,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }

                ui.add_space(14.0);
                ui.label(
                    RichText::new(tr::lng_passkey_cable_login_title())
                        .size(19.5)
                        .strong()
                        .color(palette.window_bold_fg),
                );
                ui.add_space(6.0);
                let about_w = card_width.min(310.0);
                ui.allocate_ui_with_layout(
                    Vec2::new(about_w, 0.0),
                    Layout::top_down(Align::Center),
                    |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(tr::lng_passkey_cable_login_about())
                                    .size(13.0)
                                    .color(palette.window_sub_text_fg),
                            )
                            .wrap(),
                        );
                        ui.add_space(8.0);
                        let (status_text, status_color) = match &self.hybrid_qr_status {
                            PasskeyHybridQrStatus::Scanning | PasskeyHybridQrStatus::Idle => (
                                tr::lng_passkey_cable_bluetooth(),
                                palette.window_active_text_fg,
                            ),
                            PasskeyHybridQrStatus::NoBluetooth => {
                                (tr::lng_passkey_cable_no_bluetooth(), palette.attention_fg)
                            }
                            PasskeyHybridQrStatus::Connecting { tunnel_domain } => (
                                format!("{} ({tunnel_domain})", tr::lng_passkey_cable_connecting()),
                                Color32::from_rgb(0x1E, 0x82, 0x42),
                            ),
                            PasskeyHybridQrStatus::Completed => (
                                "Passkey verified".to_owned(),
                                Color32::from_rgb(0x1E, 0x82, 0x42),
                            ),
                            PasskeyHybridQrStatus::Error(err) => (
                                format!("{} ({err})", tr::lng_passkey_cable_error()),
                                palette.attention_fg,
                            ),
                        };
                        ui.add(
                            egui::Label::new(
                                RichText::new(status_text)
                                    .size(12.5)
                                    .strong()
                                    .color(status_color),
                            )
                            .wrap(),
                        );
                    },
                );
                ui.add_space(12.0);
                if link_button(ui, &tr::lng_cancel(), palette).clicked() {
                    action = Some(QrStepAction::CancelPasskeyHybridQr);
                }
            });
            return action;
        }

        // Check expiry-based token auto-refresh (`QrWidget::_refreshTimer`).
        if let Some(deadline) = self.next_refresh_at {
            if now >= deadline {
                self.next_refresh_at = None;
                action = Some(QrStepAction::RefreshToken);
            } else {
                ui.ctx().request_repaint_after(
                    deadline
                        .saturating_duration_since(now)
                        .min(Duration::from_secs(1)),
                );
            }
        }

        // Rebuild QR texture whenever `qr_url` changes.
        if !qr_state.qr_url.is_empty() && qr_state.qr_url != self.cached_url {
            let center_size = INTRO_QR_CENTER_SIZE * 2;
            let icon_rgba = icon_cache.rgba(center_size).to_vec();
            if let Some((rgba, w, h)) = render_tdesktop_qr_rgba(
                &qr_state.qr_url,
                INTRO_QR_MAX_SIZE,
                2,
                Some((&icon_rgba, center_size)),
            ) {
                let img = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                self.texture = Some(ui.ctx().load_texture(
                    "intro_qr_code",
                    img,
                    TextureOptions::LINEAR,
                ));
                self.cached_url = qr_state.qr_url.clone();
            }
        }

        ui.with_layout(Layout::top_down(Align::Center), |ui| {
            // White rounded QR card (`introQrMaxSize + 2 * introQrBackgroundSkip`)
            let qr_logical = INTRO_QR_MAX_SIZE as f32;
            let card_side = qr_logical + 2.0 * INTRO_QR_BG_SKIP;
            let (card_rect, _) = ui.allocate_exact_size(Vec2::splat(card_side), Sense::hover());

            let painter = ui.painter();
            painter.rect_filled(
                card_rect,
                CornerRadius::same(INTRO_QR_BG_RADIUS),
                Color32::WHITE,
            );
            painter.rect_stroke(
                card_rect,
                CornerRadius::same(INTRO_QR_BG_RADIUS),
                Stroke::new(1.0, palette.divider),
                StrokeKind::Outside,
            );

            let qr_rect = Rect::from_center_size(card_rect.center(), Vec2::splat(qr_logical));
            if let Some(tex) = &self.texture {
                painter.image(
                    tex.id(),
                    qr_rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            } else {
                // Loading radial ring around Rustgram icon while awaiting `auth.exportLoginToken`.
                let icon_tex = icon_cache.texture(ui.ctx(), INTRO_QR_CENTER_SIZE * 2);
                let icon_rect = Rect::from_center_size(
                    card_rect.center(),
                    Vec2::splat(INTRO_QR_CENTER_SIZE as f32),
                );
                painter.image(
                    icon_tex.id(),
                    icon_rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
                let elapsed = now
                    .saturating_duration_since(self.spinner_started)
                    .as_secs_f32();
                paint_loading_arc(
                    painter,
                    card_rect.center(),
                    INTRO_QR_CENTER_SIZE as f32 * 0.5 + 6.0,
                    elapsed,
                    Color32::from_rgb(0x40, 0xA7, 0xE3),
                );
                ui.ctx().request_repaint();
            }

            ui.add_space(16.0);

            // Title (`tr::lng_intro_qr_title()`)
            ui.label(
                RichText::new(tr::lng_intro_qr_title())
                    .size(19.5)
                    .strong()
                    .color(palette.window_bold_fg),
            );
            ui.add_space(14.0);

            // Numbered steps (`lng_intro_qr_step1..3`) matching `introQrLabelsWidth = 292px`
            let steps_width = card_width.min(292.0);
            let steps = [
                tr::lng_intro_qr_step1(),
                tr::lng_intro_qr_step2(),
                tr::lng_intro_qr_step3(),
            ];
            ui.allocate_ui_with_layout(
                Vec2::new(steps_width, 84.0),
                Layout::top_down(Align::LEFT),
                |ui| {
                    for (idx, step_text) in steps.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("{}.", idx + 1))
                                    .size(13.5)
                                    .strong()
                                    .color(palette.window_bold_fg),
                            );
                            ui.label(RichText::new(step_text).size(13.5).color(palette.window_fg));
                        });
                        ui.add_space(6.0);
                    }
                },
            );

            // Centered error slot (`introQrErrorTop`)
            ui.add_space(6.0);
            if !error_text.is_empty() {
                ui.label(
                    RichText::new(error_text)
                        .size(12.5)
                        .color(palette.attention_fg),
                );
                ui.add_space(8.0);
            } else {
                ui.add_space(18.0);
            }

            // "Log in using phone number" link (`tr::lng_intro_qr_phone()`)
            if link_button(ui, &tr::lng_intro_qr_phone(), palette).clicked() {
                action = Some(QrStepAction::SwitchToPhone);
            }

            // "Log in using passkey" link (`tr::lng_intro_qr_passkey()`, `intro_qr.cpp:374`)
            if self.passkey_supported {
                ui.add_space(6.0);
                if link_button(ui, &tr::lng_intro_qr_passkey(), palette).clicked() {
                    action = Some(QrStepAction::LoginWithPasskey);
                }
                ui.add_space(4.0);
                if link_button(ui, &tr::lng_passkey_cable_login_title(), palette).clicked() {
                    action = Some(QrStepAction::LoginWithPasskeyHybridQr);
                }
            }
        });

        action
    }
}

fn paint_loading_arc(
    painter: &egui::Painter,
    center: Pos2,
    radius: f32,
    elapsed_secs: f32,
    color: Color32,
) {
    let start_angle = elapsed_secs * 4.2;
    let sweep = std::f32::consts::PI * 1.15;
    let segments = 24;
    let mut points = Vec::with_capacity(segments + 1);
    for i in 0..=segments {
        let a = start_angle + sweep * (i as f32 / segments as f32);
        points.push(Pos2::new(
            center.x + radius * a.cos(),
            center.y + radius * a.sin(),
        ));
    }
    painter.add(egui::Shape::line(points, Stroke::new(2.5, color)));
}
