//! Custom Video Cover frame picker & upload preview overlay (`InputMediaUploadedDocument::video_cover`).
//!
//! Port origin: `Telegram/SourceFiles/history/view/controls/history_view_video_cover_uploader.h`
//! & `.cpp`.

use std::io::Cursor;
use std::path::Path;

use egui::accesskit::Role;
use egui::{
    Align2, Color32, ColorImage, CornerRadius, CursorIcon, FontId, Id, Painter, Pos2, Rect, Sense,
    Shape, Stroke, TextureHandle, TextureOptions, Ui, Vec2, WidgetInfo,
};
use image::{ImageFormat, Rgba, RgbaImage};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold};
use tg_ui::tg_style::st;

use crate::history::theme::HistoryColors;

/// Maximum photo side length when preparing a custom video cover (`PhotoSideLimit(true)`).
pub const VIDEO_COVER_SIDE_LIMIT: u32 = 2560;
/// Square thumbnail preview side matching `st::historyReplyPreview` (`36px`).
pub const VIDEO_COVER_PREVIEW_SIDE: f32 = 36.0;
/// Number of timeline strip sample frames rendered inside [`VideoCoverPickerState`].
const COVER_STRIP_FRAMES: usize = 8;

/// Outcome returned when closing [`VideoCoverPickerState`].
#[derive(Debug, Clone)]
pub enum VideoCoverPickerOutcome {
    /// User applied a custom cover frame (encoded JPEG bytes + optional timestamp in ms).
    Applied {
        /// Target file index in `AttachFilesBoxState`.
        file_index: usize,
        /// Candidate path key.
        file_path: String,
        /// Encoded JPEG cover image bytes for `InputMediaUploadedDocument::video_cover`.
        cover_jpeg: Vec<u8>,
        /// Selected frame timestamp in milliseconds (`None` when chosen from an external image).
        frame_ms: Option<i64>,
    },
    /// User cleared the custom cover back to the video's default frame.
    Cleared {
        /// Target file index in `AttachFilesBoxState`.
        file_index: usize,
        /// Candidate path key.
        file_path: String,
    },
    /// User cancelled without changing the cover.
    Cancelled,
}

/// Validates raw image bytes (JPEG/PNG/WebP/GIF), scales them to match `(target_w, target_h)`
/// clamped within [`VIDEO_COVER_SIDE_LIMIT`], and encodes them as JPEG bytes for Telegram upload
/// (`VideoCoverUploader::startUpload`).
pub fn prepare_cover_image_bytes(
    raw_bytes: &[u8],
    target_w: u32,
    target_h: u32,
) -> Result<Vec<u8>, String> {
    if raw_bytes.is_empty() {
        return Err(tr::lng_choose_cover_bad());
    }
    let decoded = image::load_from_memory(raw_bytes).map_err(|_| tr::lng_choose_cover_bad())?;
    let (tw, th) = clamp_cover_dimensions(target_w.max(1), target_h.max(1));
    let resized = decoded.resize_exact(tw, th, image::imageops::FilterType::Triangle);
    encode_rgb_jpeg(&resized.to_rgba8())
}

/// Clamps video cover dimensions to [`VIDEO_COVER_SIDE_LIMIT`] while preserving aspect ratio.
#[must_use]
pub fn clamp_cover_dimensions(width: u32, height: u32) -> (u32, u32) {
    let w = width.max(1);
    let h = height.max(1);
    let max_side = w.max(h);
    if max_side <= VIDEO_COVER_SIDE_LIMIT {
        return (w, h);
    }
    let scale = f64::from(VIDEO_COVER_SIDE_LIMIT) / f64::from(max_side);
    (
        (f64::from(w) * scale).round().max(1.0) as u32,
        (f64::from(h) * scale).round().max(1.0) as u32,
    )
}

/// Encodes an [`RgbaImage`] as baseline JPEG bytes.
pub fn encode_rgb_jpeg(img: &RgbaImage) -> Result<Vec<u8>, String> {
    let dyn_img = image::DynamicImage::ImageRgba8(img.clone()).to_rgb8();
    let mut out = Cursor::new(Vec::new());
    dyn_img
        .write_to(&mut out, ImageFormat::Jpeg)
        .map_err(|e| format!("failed to encode video cover JPEG: {e}"))?;
    Ok(out.into_inner())
}

/// Generates a deterministic synthetic preview frame for a video timestamp fraction `0.0..=1.0`
/// (used when a sample/offline video has no local FFmpeg file on disk).
#[must_use]
pub fn synthesize_cover_frame(width: u32, height: u32, frac: f32, seed: u8) -> RgbaImage {
    let w = width.max(16);
    let h = height.max(16);
    let f = frac.clamp(0.0, 1.0);
    let mut img = RgbaImage::new(w, h);
    let base_r = (38.0 + f * 115.0 + f32::from(seed % 5) * 14.0).clamp(20.0, 235.0);
    let base_g = (68.0 + (1.0 - f) * 90.0 + f32::from(seed % 3) * 18.0).clamp(24.0, 225.0);
    let base_b = (128.0 + (0.5 - (f - 0.5).abs()) * 90.0).clamp(40.0, 240.0);

    for y in 0..h {
        let ny = y as f32 / (h.saturating_sub(1).max(1)) as f32;
        for x in 0..w {
            let nx = x as f32 / (w.saturating_sub(1).max(1)) as f32;
            let wave = ((nx * 6.0 + f * 5.0).sin() * 0.5 + 0.5) * 28.0;
            let r = (base_r * (1.0 - 0.35 * ny) + wave).clamp(0.0, 255.0) as u8;
            let g = (base_g * (0.75 + 0.25 * nx) + wave * 0.6).clamp(0.0, 255.0) as u8;
            let b = (base_b * (0.65 + 0.35 * ny)).clamp(0.0, 255.0) as u8;
            img.put_pixel(x, y, Rgba([r, g, b, 255]));
        }
    }
    img
}

/// Paints the circular translucent uploading radial indicator over a video cover preview
/// (`VideoCoverUploader::paintUploading` in `history_view_video_cover_uploader.cpp:385-407`).
pub fn paint_uploading_radial(painter: &Painter, to: Rect, now_secs: f64, paused: bool) {
    let bg_dia = st::history_edit_cover_radial_bg.0 as f32;
    let center = to.center();
    painter.circle_filled(
        center,
        bg_dia * 0.5,
        Color32::from_rgba_unmultiplied(0, 0, 0, 140),
    );

    let period_secs = (st::history_edit_cover_radial.linear_period as f64 / 1000.0).max(0.2);
    let angle_start = if paused {
        0.0
    } else {
        (now_secs / period_secs) * std::f64::consts::TAU
    };
    let sweep = std::f64::consts::TAU * 0.68;
    let radius = (st::history_edit_cover_radial.size.width.0 as f32 * 0.5 - 1.5).max(4.0);
    let steps = 20usize;
    let mut pts = Vec::with_capacity(steps + 1);
    for i in 0..=steps {
        let a = angle_start + sweep * (i as f64 / steps as f64);
        pts.push(Pos2::new(
            center.x + radius * a.cos() as f32,
            center.y + radius * a.sin() as f32,
        ));
    }
    painter.add(Shape::line(pts, Stroke::new(2.0, Color32::WHITE)));
}

/// Interactive Custom Video Cover Picker modal hosted inside `AttachFilesBoxState`.
///
/// Lets the user scrub a frame from the video timeline, import a custom image file (validated
/// with `lng_choose_cover_bad`), or reset to the default video thumbnail.
pub struct VideoCoverPickerState {
    /// Target candidate index in `AttachFilesBoxState`.
    pub file_index: usize,
    /// Video candidate path/name.
    pub file_path: String,
    /// Video width in pixels.
    pub width: u32,
    /// Video height in pixels.
    pub height: u32,
    /// Video duration in milliseconds.
    pub duration_ms: i64,
    /// Selected frame timestamp in milliseconds (`0..=duration_ms`).
    pub selected_ms: i64,
    /// Optional custom image bytes uploaded from disk instead of a video frame.
    pub custom_image_jpeg: Option<Vec<u8>>,
    /// True while simulating/running cover upload preparation.
    pub uploading: bool,
    /// Optional validation error message (e.g. `lng_choose_cover_bad`).
    pub error_message: Option<String>,
    /// Deterministic procedural seed when no local FFmpeg file is present.
    seed: u8,
    /// Cached preview texture for the active selection.
    preview_tex: Option<TextureHandle>,
    preview_key: (i64, bool),
    /// Cached timeline strip thumbnails.
    strip_textures: Vec<TextureHandle>,
}

impl std::fmt::Debug for VideoCoverPickerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoCoverPickerState")
            .field("file_index", &self.file_index)
            .field("file_path", &self.file_path)
            .field("selected_ms", &self.selected_ms)
            .field("has_custom_image", &self.custom_image_jpeg.is_some())
            .field("uploading", &self.uploading)
            .finish()
    }
}

impl VideoCoverPickerState {
    /// Opens a cover picker for a video candidate.
    #[must_use]
    pub fn open_for_video(
        file_index: usize,
        file_path: impl Into<String>,
        width: i32,
        height: i32,
        duration_ms: i64,
        existing_cover_jpeg: Option<Vec<u8>>,
        seed: u8,
    ) -> Self {
        Self {
            file_index,
            file_path: file_path.into(),
            width: width.max(160) as u32,
            height: height.max(90) as u32,
            duration_ms: duration_ms.max(1_000),
            selected_ms: 0,
            custom_image_jpeg: existing_cover_jpeg,
            uploading: false,
            error_message: None,
            seed,
            preview_tex: None,
            preview_key: (-1, false),
            strip_textures: Vec::new(),
        }
    }

    /// Loads and validates custom cover image bytes (e.g. chosen from disk or in a test).
    pub fn apply_custom_image_bytes(&mut self, raw_bytes: &[u8]) -> Result<(), String> {
        match prepare_cover_image_bytes(raw_bytes, self.width, self.height) {
            Ok(jpeg) => {
                self.custom_image_jpeg = Some(jpeg);
                self.error_message = None;
                self.preview_tex = None;
                Ok(())
            }
            Err(err) => {
                self.error_message = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Exports the currently selected cover (custom image or extracted/synthesized frame at
    /// `selected_ms`) as JPEG bytes ready for `InputMediaUploadedDocument::video_cover`.
    pub fn export_selected_cover_jpeg(&self) -> Result<Vec<u8>, String> {
        if let Some(ref jpeg) = self.custom_image_jpeg {
            return Ok(jpeg.clone());
        }
        let img = self.extract_frame_rgba(self.selected_ms, 320, 180);
        encode_rgb_jpeg(&img)
    }

    fn extract_frame_rgba(&self, ms: i64, target_w: u32, target_h: u32) -> RgbaImage {
        let p = Path::new(&self.file_path);
        if p.is_file()
            && tg_editor::video::is_ffmpeg_available()
            && let Ok(mut model) = tg_editor::video::VideoEditorModel::open_file(
                p,
                tg_editor::video::VideoEditorData::default(),
                tg_editor::video::VideoModifications::default(),
            )
        {
            model.timeline.set_cover(ms);
            if let Ok(img) = model.extract_cover_preview(target_w.max(target_h)) {
                return img;
            }
        }
        let frac = (ms as f32 / self.duration_ms.max(1) as f32).clamp(0.0, 1.0);
        synthesize_cover_frame(target_w, target_h, frac, self.seed)
    }

    fn ensure_textures(&mut self, ctx: &egui::Context) {
        if self.strip_textures.is_empty() {
            for i in 0..COVER_STRIP_FRAMES {
                let frac = i as f32 / (COVER_STRIP_FRAMES.saturating_sub(1).max(1)) as f32;
                let ms = (frac * self.duration_ms as f32).round() as i64;
                let img = self.extract_frame_rgba(ms, 48, 32);
                let ci = ColorImage::from_rgba_unmultiplied(
                    [img.width() as usize, img.height() as usize],
                    img.as_raw(),
                );
                self.strip_textures.push(ctx.load_texture(
                    format!("tg_vcover_strip_{}_{i}", self.file_index),
                    ci,
                    TextureOptions::LINEAR,
                ));
            }
        }
        let key = (self.selected_ms, self.custom_image_jpeg.is_some());
        if self.preview_tex.is_none() || self.preview_key != key {
            self.preview_key = key;
            let img = if let Some(ref jpeg) = self.custom_image_jpeg
                && let Ok(dec) = image::load_from_memory(jpeg)
            {
                dec.to_rgba8()
            } else {
                self.extract_frame_rgba(self.selected_ms, 320, 180)
            };
            let ci = ColorImage::from_rgba_unmultiplied(
                [img.width() as usize, img.height() as usize],
                img.as_raw(),
            );
            self.preview_tex = Some(ctx.load_texture(
                format!("tg_vcover_preview_{}", self.file_index),
                ci,
                TextureOptions::LINEAR,
            ));
        }
    }

    /// Renders the responsive Custom Video Cover Picker modal (adapts cleanly down to 390px width).
    pub fn show(&mut self, ui: &mut Ui, colors: &HistoryColors) -> Option<VideoCoverPickerOutcome> {
        self.ensure_textures(ui.ctx());
        let mut outcome = None;
        let screen = ui.ctx().viewport_rect();
        let modal_w = (screen.width() - 24.0).clamp(280.0, 420.0);

        egui::Area::new(Id::unique(("tg_video_cover_modal", "backdrop")))
            .order(egui::Order::Middle)
            .fixed_pos(screen.min)
            .show(ui.ctx(), |ui| {
                let (r, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
                ui.painter().rect_filled(
                    r,
                    CornerRadius::ZERO,
                    Color32::from_rgba_unmultiplied(0, 0, 0, 128),
                );
                if resp.clicked() {
                    outcome = Some(VideoCoverPickerOutcome::Cancelled);
                }
            });

        egui::Area::new(Id::unique("tg_video_cover_modal"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                egui::Frame::NONE
                    .fill(colors.bar_bg)
                    .stroke(Stroke::new(1.0, colors.divider))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(egui::Margin::same(16))
                    .show(ui, |ui| {
                        ui.set_width(modal_w - 32.0);
                        ui.label(
                            egui::RichText::new(tr::lng_choose_cover())
                                .font(FontId::new(16.0, family_semibold()))
                                .color(colors.bold_fg),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(format!(
                                "{} · {}×{}",
                                self.file_path, self.width, self.height
                            ))
                            .font(FontId::new(12.0, family_regular()))
                            .color(colors.sub_text_fg),
                        );
                        ui.add_space(10.0);

                        // Preview canvas (16:9 letterboxed inside modal width)
                        let preview_w = (modal_w - 32.0).max(200.0);
                        let preview_h = (preview_w * 9.0 / 16.0).clamp(112.0, 200.0);
                        let (preview_rect, _) =
                            ui.allocate_exact_size(Vec2::new(preview_w, preview_h), Sense::hover());
                        ui.painter().rect_filled(
                            preview_rect,
                            CornerRadius::same(8),
                            Color32::BLACK,
                        );
                        if let Some(ref tex) = self.preview_tex {
                            ui.painter().image(
                                tex.id(),
                                preview_rect,
                                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                Color32::WHITE,
                            );
                        }
                        if self.uploading {
                            let now_secs = ui.ctx().input(|i| i.time);
                            paint_uploading_radial(ui.painter(), preview_rect, now_secs, false);
                            ui.ctx().request_repaint();
                        }

                        // Timestamp badge overlay
                        let badge_label = if self.custom_image_jpeg.is_some() {
                            "Custom photo".to_owned()
                        } else {
                            let secs = self.selected_ms as f64 / 1000.0;
                            format!("Frame {secs:.1}s")
                        };
                        let badge_g = ui.painter().layout_no_wrap(
                            badge_label,
                            FontId::new(11.0, family_semibold()),
                            Color32::WHITE,
                        );
                        let badge_r = Rect::from_min_size(
                            Pos2::new(preview_rect.left() + 8.0, preview_rect.bottom() - 24.0),
                            Vec2::new(badge_g.size().x + 12.0, 18.0),
                        );
                        ui.painter().rect_filled(
                            badge_r,
                            CornerRadius::same(6),
                            Color32::from_rgba_unmultiplied(0, 0, 0, 155),
                        );
                        ui.painter().galley(
                            Pos2::new(badge_r.left() + 6.0, badge_r.top() + 2.0),
                            badge_g,
                            Color32::WHITE,
                        );

                        ui.add_space(10.0);

                        // Interactive frame scrubber strip
                        let strip_h = 40.0;
                        let (strip_rect, strip_resp) = ui.allocate_exact_size(
                            Vec2::new(preview_w, strip_h),
                            Sense::click_and_drag(),
                        );
                        strip_resp.widget_info(|| {
                            WidgetInfo::slider(
                                true,
                                self.selected_ms as f64,
                                "Select video cover frame",
                            )
                        });
                        if strip_resp.hovered() || strip_resp.dragged() {
                            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
                        }
                        let cell_w = strip_rect.width() / COVER_STRIP_FRAMES as f32;
                        for (i, tex) in self.strip_textures.iter().enumerate() {
                            let cr = Rect::from_min_size(
                                Pos2::new(strip_rect.left() + i as f32 * cell_w, strip_rect.top()),
                                Vec2::new(cell_w, strip_h),
                            );
                            ui.painter().image(
                                tex.id(),
                                cr,
                                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                Color32::WHITE,
                            );
                        }
                        ui.painter().rect_stroke(
                            strip_rect,
                            CornerRadius::same(6),
                            Stroke::new(1.0, colors.divider),
                            egui::StrokeKind::Inside,
                        );

                        if (strip_resp.clicked() || strip_resp.dragged())
                            && let Some(pos) = strip_resp.interact_pointer_pos()
                        {
                            let frac = ((pos.x - strip_rect.left()) / strip_rect.width().max(1.0))
                                .clamp(0.0, 1.0);
                            self.selected_ms = (frac * self.duration_ms as f32).round() as i64;
                            self.custom_image_jpeg = None;
                            self.error_message = None;
                        }

                        // Current frame indicator handle on strip
                        let head_frac = (self.selected_ms as f32 / self.duration_ms.max(1) as f32)
                            .clamp(0.0, 1.0);
                        let head_x = strip_rect.left() + head_frac * strip_rect.width();
                        let handle_rect = Rect::from_center_size(
                            Pos2::new(
                                head_x.clamp(strip_rect.left() + 4.0, strip_rect.right() - 4.0),
                                strip_rect.center().y,
                            ),
                            Vec2::new(6.0, strip_h + 4.0),
                        );
                        ui.painter()
                            .rect_filled(handle_rect, CornerRadius::same(3), colors.accent);
                        ui.painter().rect_stroke(
                            handle_rect,
                            CornerRadius::same(3),
                            Stroke::new(1.5, Color32::WHITE),
                            egui::StrokeKind::Inside,
                        );

                        if let Some(ref err) = self.error_message {
                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new(err)
                                    .size(11.5)
                                    .color(Color32::from_rgb(0xD9, 0x3A, 0x3A)),
                            );
                        }

                        ui.add_space(12.0);
                        ui.horizontal_wrapped(|ui| {
                            let clear_btn = ui.small_button("Reset Cover");
                            clear_btn.widget_info(|| {
                                WidgetInfo::labeled(Role::Button, true, "Reset video cover")
                            });
                            if clear_btn.clicked() {
                                outcome = Some(VideoCoverPickerOutcome::Cleared {
                                    file_index: self.file_index,
                                    file_path: self.file_path.clone(),
                                });
                            }

                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let apply_btn = egui::Button::new(
                                        egui::RichText::new("Use Cover")
                                            .color(Color32::WHITE)
                                            .strong(),
                                    )
                                    .fill(colors.accent)
                                    .corner_radius(CornerRadius::same(6));
                                    let apply_resp = ui.add(apply_btn);
                                    apply_resp.widget_info(|| {
                                        WidgetInfo::labeled(
                                            Role::Button,
                                            true,
                                            "Use selected cover",
                                        )
                                    });
                                    if apply_resp.clicked() {
                                        match self.export_selected_cover_jpeg() {
                                            Ok(cover_jpeg) => {
                                                let frame_ms = self
                                                    .custom_image_jpeg
                                                    .is_none()
                                                    .then_some(self.selected_ms);
                                                outcome = Some(VideoCoverPickerOutcome::Applied {
                                                    file_index: self.file_index,
                                                    file_path: self.file_path.clone(),
                                                    cover_jpeg,
                                                    frame_ms,
                                                });
                                            }
                                            Err(err) => {
                                                self.error_message = Some(err);
                                            }
                                        }
                                    }

                                    ui.add_space(6.0);
                                    if ui.button("Cancel").clicked() {
                                        outcome = Some(VideoCoverPickerOutcome::Cancelled);
                                    }
                                },
                            );
                        });
                    });
            });

        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_cover_dimensions_respects_side_limit() {
        assert_eq!(clamp_cover_dimensions(1280, 720), (1280, 720));
        assert_eq!(clamp_cover_dimensions(3840, 2160), (2560, 1440));
        assert_eq!(clamp_cover_dimensions(2160, 3840), (1440, 2560));
    }

    #[test]
    fn prepare_cover_rejects_invalid_bytes_and_encodes_valid_image() {
        let err = prepare_cover_image_bytes(b"not an image", 1280, 720).unwrap_err();
        assert_eq!(err, tr::lng_choose_cover_bad());

        let synth = synthesize_cover_frame(64, 36, 0.5, 3);
        let mut png = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(synth)
            .write_to(&mut png, ImageFormat::Png)
            .expect("encode png");
        let jpeg = prepare_cover_image_bytes(&png.into_inner(), 320, 180).expect("valid cover");
        assert!(jpeg.starts_with(&[0xFF, 0xD8]));
    }
}
