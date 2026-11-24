//! Interactive Photo Crop modal box backed by `tg-editor`'s crop and userpic math.
//!
//! Port origin: `Telegram/SourceFiles/editor/photo_editor_layer_widget.cpp`,
//! `Telegram/SourceFiles/editor/editor_crop.cpp`,
//! and `Telegram/SourceFiles/boxes/photo_crop_box.cpp`.

use egui::{
    Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, TextureHandle, Ui, Vec2,
};
use image::RgbaImage;
use tg_editor::{
    CropController, CropCursor, CropMode, CropPreset, CropType, EditorData, EditorError,
    PhotoModifications, PointF, Rect as EdRect, Size as EdSize, SizeF, finalize_profile_photo,
    flip_size_f_by_rotation, prepare_profile_photo_input,
};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, font_id_from_style};
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{Colors, RoundButtonStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::buttons::RoundButton;

use crate::boxes::strings;

/// State for the Photo Crop modal powered by `tg_editor::CropController`.
pub struct PhotoCropBoxState {
    /// Dialog heading (e.g. `"Crop Profile Photo"`).
    pub title: String,
    /// Prepared source image (`>= 640px` per `tg_editor::prepare_profile_photo_input`).
    pub source_image: RgbaImage,
    /// Non-destructive modification parameters (angle, flip, crop rect, shape).
    pub modifications: PhotoModifications,
    /// Pure crop geometry and drag controller from `tg-editor`.
    pub controller: CropController,
    /// Selected aspect-ratio preset.
    pub preset: CropPreset,
    /// Cached egui texture of the source image for live canvas rendering.
    texture: Option<TextureHandle>,
    /// Last applied canvas transform parameters to avoid redundant resets.
    last_canvas_size: Option<(i32, i32, i32, bool)>,
    /// Error message if crop/export fails.
    pub error: Option<String>,
}

impl std::fmt::Debug for PhotoCropBoxState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhotoCropBoxState")
            .field("title", &self.title)
            .field(
                "image_dims",
                &(self.source_image.width(), self.source_image.height()),
            )
            .field("modifications", &self.modifications)
            .field("preset", &self.preset)
            .finish()
    }
}

impl PhotoCropBoxState {
    /// Creates a profile photo crop box from an `RgbaImage` using `tg_editor::prepare_profile_photo_input`.
    pub fn for_profile_photo(
        title: impl Into<String>,
        image: &RgbaImage,
        crop_type: CropType,
    ) -> Result<Self, EditorError> {
        let editor_data = EditorData {
            crop_type,
            crop_mode: CropMode::Hint,
            keep_aspect_ratio: true,
            fixed_crop: true,
            ..EditorData::default()
        };
        let prepared = prepare_profile_photo_input(image, editor_data)?;
        let img_size = EdSize::new(
            prepared.image.width() as i32,
            prepared.image.height() as i32,
        );
        let controller =
            CropController::new(&prepared.modifications, img_size, prepared.editor_data);
        Ok(Self {
            title: title.into(),
            source_image: prepared.image,
            modifications: prepared.modifications,
            controller,
            preset: CropPreset::Square,
            texture: None,
            last_canvas_size: None,
            error: None,
        })
    }

    /// Creates a profile photo crop box preloaded with a clean synthetic sample gradient image
    /// (useful for offline demos and unit tests without real photos).
    #[must_use]
    pub fn sample_profile_photo(title: impl Into<String>, accent_rgb: [u8; 3]) -> Self {
        let sample = generate_sample_avatar_image(720, 540, accent_rgb);
        // Safe: 720x540 has valid 4:3 dimensions within 10:1 limit.
        match Self::for_profile_photo(title, &sample, CropType::Ellipse) {
            Ok(state) => state,
            Err(_) => {
                let fallback =
                    RgbaImage::from_pixel(640, 640, image::Rgba([0x40, 0xA7, 0xE3, 255]));
                let mods = PhotoModifications::default();
                let controller = CropController::new(
                    &mods,
                    EdSize::new(640, 640),
                    EditorData {
                        crop_type: CropType::Ellipse,
                        keep_aspect_ratio: true,
                        fixed_crop: true,
                        ..EditorData::default()
                    },
                );
                Self {
                    title: strings::CROP_PHOTO_FALLBACK_TITLE.to_owned(),
                    source_image: fallback,
                    modifications: mods,
                    controller,
                    preset: CropPreset::Square,
                    texture: None,
                    last_canvas_size: None,
                    error: None,
                }
            }
        }
    }

    /// Rotates the photo clockwise by 90 degrees via `PhotoModifications::rotate_90_cw`.
    pub fn rotate_90_cw(&mut self) {
        self.modifications.rotate_90_cw();
        self.last_canvas_size = None;
    }

    /// Flips the photo horizontally via `PhotoModifications::toggle_flip`.
    pub fn toggle_flip(&mut self) {
        self.modifications.toggle_flip();
        self.last_canvas_size = None;
    }

    /// Applies an aspect-ratio [`CropPreset`] to `tg_editor::CropController`.
    pub fn set_preset(&mut self, preset: CropPreset) {
        self.preset = preset;
        let img_size = EdSize::new(
            self.source_image.width() as i32,
            self.source_image.height() as i32,
        );
        let ratio = preset.ratio(img_size);
        self.controller.set_aspect_ratio(ratio);
        self.modifications.crop = self.controller.save_crop_rect();
    }

    /// Resets rotation, flip, and crop back to the initial centered square.
    pub fn reset(&mut self) {
        let w = self.source_image.width() as i32;
        let h = self.source_image.height() as i32;
        let min_side = w.min(h);
        let initial_crop = EdRect::new((w - min_side) / 2, (h - min_side) / 2, min_side, min_side);
        self.modifications.angle = 0;
        self.modifications.flipped = false;
        self.modifications.crop = Some(initial_crop);
        let data = self.controller.editor_data().clone();
        self.controller = CropController::new(&self.modifications, EdSize::new(w, h), data);
        self.preset = CropPreset::Square;
        self.last_canvas_size = None;
    }

    /// Produces the final cropped/rotated/flipped profile `RgbaImage` via `tg_editor::finalize_profile_photo`.
    pub fn finalize(&mut self) -> Result<RgbaImage, EditorError> {
        self.modifications.crop = Some(self.controller.crop_rect());
        finalize_profile_photo(&self.source_image, &self.modifications)
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        BoxModal::new(id, &self.title)
            .width(st::box_wide_width.0 as f32 + 60.0)
            .with_left_button(BoxButton::default_btn(
                "reset",
                tr::lng_profile_photo_reset_button(),
            ))
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::default_btn("save", tr::lng_settings_save()))
    }

    /// Renders the interactive crop canvas, handle dragging, and transform controls.
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) {
        let canvas_w = ui.available_width().max(260.0);
        let canvas_h = 260.0;
        let (canvas_rect, resp) =
            ui.allocate_exact_size(Vec2::new(canvas_w, canvas_h), Sense::click_and_drag());

        ui.painter().rect_filled(
            canvas_rect,
            CornerRadius::same(6),
            Color32::from_rgb(0x18, 0x1E, 0x26),
        );

        // Fit source image inside canvas with 12px margin for crop handles.
        let margin = 12.0;
        let avail_w = (canvas_w - margin * 2.0).max(40.0) as f64;
        let avail_h = (canvas_h - margin * 2.0).max(40.0) as f64;
        let raw_size = SizeF::new(
            self.source_image.width() as f64,
            self.source_image.height() as f64,
        );
        let rotated = flip_size_f_by_rotation(raw_size, self.modifications.angle);
        let scale = (avail_w / rotated.width).min(avail_h / rotated.height);
        let scaled_unrotated = SizeF::new(raw_size.width * scale, raw_size.height * scale);
        let disp_w = (rotated.width * scale) as f32;
        let disp_h = (rotated.height * scale) as f32;
        let img_pos_x = (canvas_w - disp_w) * 0.5;
        let img_pos_y = (canvas_h - disp_h) * 0.5;

        let key = (
            canvas_w.round() as i32,
            canvas_h.round() as i32,
            self.modifications.angle,
            self.modifications.flipped,
        );
        if self.last_canvas_size != Some(key) {
            self.controller.apply_transform(
                EdRect::new(0, 0, canvas_w.round() as i32, canvas_h.round() as i32),
                PointF::new(f64::from(img_pos_x), f64::from(img_pos_y)),
                self.modifications.angle,
                self.modifications.flipped,
                scaled_unrotated,
            );
            self.last_canvas_size = Some(key);
        }

        // Upload/cache preview texture
        let tex = self.texture.get_or_insert_with(|| {
            let color_img = egui::ColorImage::from_rgba_unmultiplied(
                [
                    self.source_image.width() as usize,
                    self.source_image.height() as usize,
                ],
                self.source_image.as_raw(),
            );
            ui.ctx().load_texture(
                "tg_photo_crop_preview",
                color_img,
                egui::TextureOptions::LINEAR,
            )
        });

        let img_rect = Rect::from_min_size(
            Pos2::new(canvas_rect.min.x + img_pos_x, canvas_rect.min.y + img_pos_y),
            Vec2::new(disp_w, disp_h),
        );
        let uv = if self.modifications.flipped {
            Rect::from_min_max(Pos2::new(1.0, 0.0), Pos2::new(0.0, 1.0))
        } else {
            Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0))
        };
        ui.painter().image(tex.id(), img_rect, uv, Color32::WHITE);

        // Wire pointer interaction directly to `tg_editor::CropController`.
        if let Some(pointer) = resp.hover_pos().or_else(|| resp.interact_pointer_pos()) {
            let local = PointF::new(
                f64::from(pointer.x - canvas_rect.min.x),
                f64::from(pointer.y - canvas_rect.min.y),
            );
            let edge = self.controller.hit_test(local);
            let cursor = match CropController::cursor_for_edge(edge) {
                CropCursor::SizeAll => CursorIcon::Grab,
                CropCursor::SizeHor => CursorIcon::ResizeHorizontal,
                CropCursor::SizeVer => CursorIcon::ResizeVertical,
                CropCursor::SizeFDiag => CursorIcon::ResizeNwSe,
                CropCursor::SizeBDiag => CursorIcon::ResizeNeSw,
                CropCursor::Default => CursorIcon::Default,
            };
            if cursor != CursorIcon::Default {
                ui.ctx().set_cursor_icon(cursor);
            }
            if resp.drag_started() {
                self.controller.begin_drag(local, false);
            }
            if resp.dragged() && self.controller.is_dragging() {
                self.controller.update_drag(local);
            }
        }
        if resp.drag_stopped() && self.controller.is_dragging() {
            self.controller.end_drag();
            self.modifications.crop = self.controller.save_crop_rect();
        }

        // Paint darkened exterior scrim around the crop rectangle.
        let cp = self.controller.crop_paint_f();
        let crop_rect = Rect::from_min_size(
            Pos2::new(
                canvas_rect.min.x + cp.x as f32,
                canvas_rect.min.y + cp.y as f32,
            ),
            Vec2::new(cp.width as f32, cp.height as f32),
        );
        let scrim = Color32::from_rgba_unmultiplied(0, 0, 0, 130);
        // Top, bottom, left, right scrim bands around crop_rect
        ui.painter().rect_filled(
            Rect::from_min_max(
                img_rect.min,
                Pos2::new(img_rect.max.x, crop_rect.min.y.max(img_rect.min.y)),
            ),
            CornerRadius::ZERO,
            scrim,
        );
        ui.painter().rect_filled(
            Rect::from_min_max(
                Pos2::new(img_rect.min.x, crop_rect.max.y.min(img_rect.max.y)),
                img_rect.max,
            ),
            CornerRadius::ZERO,
            scrim,
        );
        ui.painter().rect_filled(
            Rect::from_min_max(
                Pos2::new(img_rect.min.x, crop_rect.min.y),
                Pos2::new(crop_rect.min.x.max(img_rect.min.x), crop_rect.max.y),
            ),
            CornerRadius::ZERO,
            scrim,
        );
        ui.painter().rect_filled(
            Rect::from_min_max(
                Pos2::new(crop_rect.max.x.min(img_rect.max.x), crop_rect.min.y),
                Pos2::new(img_rect.max.x, crop_rect.max.y),
            ),
            CornerRadius::ZERO,
            scrim,
        );

        // Rule-of-thirds guides + frame + optional circular avatar preview ring
        ui.painter().rect_stroke(
            crop_rect,
            CornerRadius::ZERO,
            Stroke::new(1.5, Color32::WHITE),
            egui::StrokeKind::Inside,
        );
        let grid_stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 90));
        for i in 1..=2 {
            let fx = crop_rect.min.x + crop_rect.width() * (i as f32 / 3.0);
            let fy = crop_rect.min.y + crop_rect.height() * (i as f32 / 3.0);
            ui.painter().line_segment(
                [
                    Pos2::new(fx, crop_rect.min.y),
                    Pos2::new(fx, crop_rect.max.y),
                ],
                grid_stroke,
            );
            ui.painter().line_segment(
                [
                    Pos2::new(crop_rect.min.x, fy),
                    Pos2::new(crop_rect.max.x, fy),
                ],
                grid_stroke,
            );
        }
        if self.modifications.crop_type == CropType::Ellipse {
            let r = crop_rect.width().min(crop_rect.height()) * 0.5;
            ui.painter().circle_stroke(
                crop_rect.center(),
                r,
                Stroke::new(1.8, Color32::from_rgba_unmultiplied(255, 255, 255, 210)),
            );
        }

        // Paint corner handle squares
        for corner in [
            crop_rect.left_top(),
            crop_rect.right_top(),
            crop_rect.left_bottom(),
            crop_rect.right_bottom(),
        ] {
            let hrect = Rect::from_center_size(corner, Vec2::splat(9.0));
            ui.painter()
                .rect_filled(hrect, CornerRadius::same(2), Color32::WHITE);
            ui.painter().rect_stroke(
                hrect,
                CornerRadius::same(2),
                Stroke::new(1.0, colors.color("windowBgActive")),
                egui::StrokeKind::Inside,
            );
        }

        // Controls bar: Rotate, Flip, and Aspect presets
        ui.add_space(10.0);
        let light_st = RoundButtonStyle::default_light(colors);
        let flip_lbl = tr::lng_photo_editor_menu_flip();
        let orig_lbl = tr::lng_photo_editor_crop_original();
        let free_lbl = tr::lng_photo_editor_crop_free();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
            if RoundButton::new(strings::CROP_ROTATE_90, light_st)
                .show(ui)
                .clicked()
            {
                self.rotate_90_cw();
            }
            if RoundButton::new(&flip_lbl, light_st).show(ui).clicked() {
                self.toggle_flip();
            }
            for (preset, label) in [
                (CropPreset::Square, strings::CROP_PRESET_1_1),
                (CropPreset::Original, orig_lbl.as_str()),
                (CropPreset::Free, free_lbl.as_str()),
            ] {
                if ui.selectable_label(self.preset == preset, label).clicked() {
                    self.set_preset(preset);
                }
            }
        });

        let saved = self.controller.crop_rect();
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(format!(
                "Crop: {}×{} px · Rotation: {}°{}",
                saved.width,
                saved.height,
                self.modifications.angle,
                if self.modifications.flipped {
                    strings::CROP_FLIPPED_TAG
                } else {
                    ""
                }
            ))
            .font(FontId::new(11.5, family_regular()))
            .color(colors.color("windowSubTextFg")),
        );

        if let Some(ref err) = self.error {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(err)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("boxTextFgError")),
            );
        }
    }
}

fn generate_sample_avatar_image(width: u32, height: u32, accent: [u8; 3]) -> RgbaImage {
    let mut img = RgbaImage::new(width, height);
    let cx = width as f32 * 0.5;
    let cy = height as f32 * 0.5;
    let max_d = (cx * cx + cy * cy).sqrt().max(1.0);
    for y in 0..height {
        for x in 0..width {
            let fx = x as f32 / width as f32;
            let fy = y as f32 / height as f32;
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let radial = (1.0 - (dx * dx + dy * dy).sqrt() / max_d).clamp(0.0, 1.0);
            let r = ((f32::from(accent[0]) * (0.65 + 0.35 * radial) + fx * 35.0).clamp(0.0, 255.0))
                as u8;
            let g = ((f32::from(accent[1]) * (0.60 + 0.40 * (1.0 - fy)) + radial * 28.0)
                .clamp(0.0, 255.0)) as u8;
            let b = ((f32::from(accent[2]) * (0.70 + 0.30 * fy) + (1.0 - fx) * 30.0)
                .clamp(0.0, 255.0)) as u8;
            img.put_pixel(x, y, image::Rgba([r, g, b, 255]));
        }
    }
    img
}
