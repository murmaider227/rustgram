//! New account sign-up step: first/last name, circular photo picker, and Terms of Service box.
//!
//! Port origin: `Telegram/SourceFiles/intro/intro_signup.cpp` and
//! `Telegram/SourceFiles/intro/intro_widget.cpp` (`showTerms`, `acceptTerms`).

use std::path::Path;
use std::time::Instant;

use egui::{
    Align, Color32, ColorImage, CornerRadius, FontId, Frame, Id, Key, Layout, Margin, Order, Pos2,
    Rect, RichText, ScrollArea, Sense, Stroke, TextEdit, TextureHandle, TextureOptions, Ui, Vec2,
};
use tg_lang::tr;
use tg_tl::types::help::TermsOfService;

use crate::intro::style::{
    INTRO_NEXT_BUTTON_WIDTH, ShakeAnimation, link_button, primary_intro_button, underline_input,
};
use tg_app_core::stand_in::theme::Palette;

/// Chosen profile avatar during SignUp (`SignupWidget::_photo`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignupPhoto {
    /// No photo selected yet (camera badge shown).
    None,
    /// Built-in geometric color preset (GPL-clean synthetic avatar).
    PresetColor {
        /// Preset label.
        label: &'static str,
        /// Fill color.
        rgb: [u8; 3],
    },
    /// Loaded from a local PNG/JPEG file path.
    LocalImage {
        /// Source file path.
        path: String,
        /// Square circular-masked RGBA pixel dimensions.
        size: u32,
        /// Straight RGBA pixel buffer.
        rgba: Vec<u8>,
    },
}

/// Active Terms of Service modal state (`Intro::Widget::showTerms`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermsModalState {
    /// No Terms modal open.
    Closed,
    /// Showing the Terms of Service text (`popup_confirm = true` when triggered by SignUp submit).
    ShowingTerms {
        /// Whether agreeing should immediately continue submitting SignUp.
        submit_on_agree: bool,
    },
    /// User clicked Decline and is shown `lng_terms_signup_sorry`.
    DeclinedSorry {
        /// Whether returning to Terms should still submit on agree.
        submit_on_agree: bool,
    },
}

/// Actions emitted by [`SignupStepState::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignupStepAction {
    /// User accepted the Terms of Service popup.
    AcceptTerms,
    /// Submit `auth.signUp` with `(first_name, last_name)`.
    Submit {
        /// First name.
        first_name: String,
        /// Last name.
        last_name: String,
    },
}

/// UI state for `LoginStep::SignUp` (`Intro::SignupWidget`).
pub struct SignupStepState {
    /// First name input text.
    pub first_name: String,
    /// Last name input text.
    pub last_name: String,
    /// Chosen avatar photo.
    pub photo: SignupPhoto,
    photo_texture: Option<TextureHandle>,
    /// Whether the photo picker dialog is open.
    pub photo_picker_open: bool,
    /// File path input inside the photo picker dialog.
    pub photo_path_draft: String,
    /// Error shown inside the photo picker dialog (e.g. unreadable image).
    pub photo_error: String,
    /// Active Terms of Service modal state.
    pub terms_modal: TermsModalState,
    /// Shake animation on invalid name error.
    pub shake: ShakeAnimation,
    /// Focus request for first name input.
    pub focus_first: bool,
    /// Focus request for last name input.
    pub focus_last: bool,
}

impl std::fmt::Debug for SignupStepState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignupStepState")
            .field("first_name", &self.first_name)
            .field("last_name", &self.last_name)
            .field("photo_picker_open", &self.photo_picker_open)
            .field("terms_modal", &self.terms_modal)
            .finish()
    }
}

impl Default for SignupStepState {
    fn default() -> Self {
        Self::new()
    }
}

impl SignupStepState {
    /// Creates a fresh `SignupStepState`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            first_name: String::new(),
            last_name: String::new(),
            photo: SignupPhoto::None,
            photo_texture: None,
            photo_picker_open: false,
            photo_path_draft: String::new(),
            photo_error: String::new(),
            terms_modal: TermsModalState::Closed,
            shake: ShakeAnimation::default(),
            focus_first: true,
            focus_last: false,
        }
    }

    /// Resets sign-up fields when entering a new `SignUp` step.
    pub fn reset(&mut self) {
        self.first_name.clear();
        self.last_name.clear();
        self.photo = SignupPhoto::None;
        self.photo_texture = None;
        self.photo_picker_open = false;
        self.photo_path_draft.clear();
        self.photo_error.clear();
        self.terms_modal = TermsModalState::Closed;
        self.shake.stop();
        self.focus_first = true;
        self.focus_last = false;
    }

    /// Loads and center-crops a local PNG/JPEG image into a circular 128x128 RGBA avatar.
    pub fn load_photo_from_path(&mut self, path: &Path) -> Result<(), String> {
        let img = image::open(path).map_err(|_| tr::lng_bad_photo())?;
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        let side = w.min(h).max(1);
        let ox = (w - side) / 2;
        let oy = (h - side) / 2;
        let cropped = image::imageops::crop_imm(&rgba, ox, oy, side, side).to_image();
        let target = 128u32;
        let resized = image::imageops::resize(
            &cropped,
            target,
            target,
            image::imageops::FilterType::Triangle,
        );

        let mut buf = resized.into_raw();
        let center = (target as f32 - 1.0) * 0.5;
        let radius = target as f32 * 0.5;
        for y in 0..target {
            for x in 0..target {
                let dx = x as f32 - center;
                let dy = y as f32 - center;
                if dx * dx + dy * dy > radius * radius {
                    let idx = ((y * target + x) * 4) as usize;
                    buf[idx + 3] = 0;
                }
            }
        }
        self.photo = SignupPhoto::LocalImage {
            path: path.display().to_string(),
            size: target,
            rgba: buf,
        };
        self.photo_texture = None;
        self.photo_error.clear();
        Ok(())
    }

    /// Triggers shake animation on name validation error.
    pub fn trigger_error(&mut self, now: Instant) {
        self.shake.trigger(now);
        self.focus_first = true;
    }

    /// Renders the SignUp step (`SignupWidget`) + Photo Picker & Terms of Service boxes.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut Ui,
        card_width: f32,
        terms: Option<&TermsOfService>,
        terms_accepted: bool,
        error_text: &str,
        now: Instant,
        palette: &Palette,
    ) -> Option<SignupStepAction> {
        let mut action = None;
        let width = card_width.min(INTRO_NEXT_BUTTON_WIDTH);
        let shake_dx = self.shake.offset(now);
        if self.shake.is_animating(now) {
            ui.ctx().request_repaint();
        }

        // Upload local photo texture lazily if needed
        if let SignupPhoto::LocalImage { size, rgba, .. } = &self.photo
            && self.photo_texture.is_none()
        {
            let ci = ColorImage::from_rgba_unmultiplied([*size as usize, *size as usize], rgba);
            self.photo_texture = Some(ui.ctx().load_texture(
                "intro_signup_avatar",
                ci,
                TextureOptions::LINEAR,
            ));
        }

        ui.with_layout(Layout::top_down(Align::LEFT), |ui| {
            // Top header row: Title + Description on left, Circular Photo Picker on right (`introPhotoTop = 10px`)
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_max_width((width - 84.0).max(150.0));
                    ui.label(
                        RichText::new(tr::lng_signup_title())
                            .size(18.0)
                            .strong()
                            .color(palette.window_bold_fg),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(tr::lng_signup_desc())
                            .size(13.5)
                            .color(palette.window_sub_text_fg),
                    );
                });

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let avatar_size = 70.0;
                    let (avatar_rect, avatar_resp) =
                        ui.allocate_exact_size(Vec2::splat(avatar_size), Sense::click());
                    if avatar_resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    let center = avatar_rect.center();
                    let radius = avatar_size * 0.5;
                    match &self.photo {
                        SignupPhoto::None => {
                            let bg = if avatar_resp.hovered() {
                                palette.window_bg_active
                            } else {
                                Color32::from_rgb(0x40, 0xA7, 0xE3)
                            };
                            ui.painter().circle_filled(center, radius, bg);
                            // Simple camera silhouette (pure geometric)
                            let body = Rect::from_center_size(
                                Pos2::new(center.x, center.y + 2.0),
                                Vec2::new(26.0, 18.0),
                            );
                            ui.painter()
                                .rect_filled(body, CornerRadius::same(3), Color32::WHITE);
                            ui.painter().circle_filled(body.center(), 5.0, bg);
                            ui.painter()
                                .circle_filled(body.center(), 2.5, Color32::WHITE);
                        }
                        SignupPhoto::PresetColor { rgb, .. } => {
                            let fill = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                            ui.painter().circle_filled(center, radius, fill);
                            let initials = signup_initials(&self.first_name, &self.last_name);
                            ui.painter().text(
                                center,
                                egui::Align2::CENTER_CENTER,
                                initials,
                                FontId::proportional(24.0),
                                Color32::WHITE,
                            );
                        }
                        SignupPhoto::LocalImage { .. } => {
                            if let Some(tex) = &self.photo_texture {
                                ui.painter().image(
                                    tex.id(),
                                    avatar_rect,
                                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                    Color32::WHITE,
                                );
                            }
                        }
                    }
                    if avatar_resp.clicked() {
                        self.photo_picker_open = true;
                        self.photo_error.clear();
                    }
                });
            });

            ui.add_space(20.0);

            let first_resp = underline_input(
                ui,
                "intro_signup_first_field",
                &mut self.first_name,
                &tr::lng_signup_firstname(),
                width,
                false,
                !error_text.is_empty(),
                shake_dx,
                palette,
            );
            if self.focus_first
                && !self.photo_picker_open
                && self.terms_modal == TermsModalState::Closed
            {
                first_resp.request_focus();
                self.focus_first = false;
            }
            let first_enter = first_resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));

            ui.add_space(10.0);

            let last_resp = underline_input(
                ui,
                "intro_signup_last_field",
                &mut self.last_name,
                &tr::lng_signup_lastname(),
                width,
                false,
                false,
                shake_dx,
                palette,
            );
            if self.focus_last
                && !self.photo_picker_open
                && self.terms_modal == TermsModalState::Closed
            {
                last_resp.request_focus();
                self.focus_last = false;
            }
            let last_enter = last_resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));

            // Centered error slot
            ui.add_space(12.0);
            ui.allocate_ui_with_layout(
                Vec2::new(width, 20.0),
                Layout::top_down(Align::Center),
                |ui| {
                    if !error_text.is_empty() {
                        ui.label(
                            RichText::new(error_text)
                                .size(12.5)
                                .color(palette.attention_fg),
                        );
                    }
                },
            );
            ui.add_space(10.0);

            // "Sign Up" primary button (`tr::lng_intro_finish()`)
            let submit_clicked =
                primary_intro_button(ui, width, &tr::lng_intro_finish(), palette).clicked();
            if first_enter && self.last_name.trim().is_empty() && !self.first_name.trim().is_empty()
            {
                self.focus_last = true;
            } else if submit_clicked || last_enter || first_enter {
                let first = self.first_name.trim();
                if first.is_empty() {
                    self.trigger_error(now);
                } else {
                    let needs_terms_popup =
                        !terms_accepted && terms.is_some_and(|t| t.popup && !t.text.is_empty());
                    if needs_terms_popup {
                        self.terms_modal = TermsModalState::ShowingTerms {
                            submit_on_agree: true,
                        };
                    } else {
                        action = Some(SignupStepAction::Submit {
                            first_name: first.to_owned(),
                            last_name: self.last_name.trim().to_owned(),
                        });
                    }
                }
            }

            // Bottom Terms of Service link (`Intro::Widget::showTerms`)
            if let Some(tos) = terms
                && !tos.text.is_empty()
            {
                ui.add_space(22.0);
                ui.allocate_ui_with_layout(
                    Vec2::new(width, 40.0),
                    Layout::top_down(Align::Center),
                    |ui| {
                        let link_text = tr::lng_terms_signup_link();
                        let full_notice = tr::lng_terms_signup(&link_text);
                        ui.label(
                            RichText::new(full_notice)
                                .size(12.5)
                                .color(palette.window_sub_text_fg),
                        );
                        if link_button(ui, &link_text, palette).clicked() {
                            self.terms_modal = TermsModalState::ShowingTerms {
                                submit_on_agree: false,
                            };
                        }
                    },
                );
            }
        });

        // Photo Picker modal
        if self.photo_picker_open {
            self.show_photo_picker_modal(ui.ctx(), palette);
        }

        // Terms of Service modal (`Window::TermsBox`)
        if let Some(tos) = terms
            && self.terms_modal != TermsModalState::Closed
            && let Some(act) = self.show_terms_modal(ui.ctx(), tos, palette)
        {
            action = Some(act);
        }

        action
    }

    fn show_photo_picker_modal(&mut self, ctx: &egui::Context, palette: &Palette) {
        let screen = ctx.content_rect();
        let mut close = ctx.input(|i| i.key_pressed(Key::Escape));

        egui::Area::new(Id::unique("intro_photo_picker_backdrop"))
            .order(Order::Foreground)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let (_, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
                ui.painter()
                    .rect_filled(screen, 0.0, palette.layer_backdrop);
                if resp.clicked() {
                    close = true;
                }
            });

        let box_w = 340.0_f32.min(screen.width() - 32.0);
        let box_pos = Pos2::new(screen.center().x - box_w * 0.5, screen.center().y - 130.0);

        egui::Area::new(Id::unique("intro_photo_picker_box"))
            .order(Order::Tooltip)
            .fixed_pos(box_pos)
            .show(ctx, |ui| {
                Frame::NONE
                    .fill(palette.window_bg)
                    .corner_radius(CornerRadius::same(8))
                    .stroke(Stroke::new(1.0, palette.divider))
                    .inner_margin(Margin::same(18))
                    .show(ui, |ui| {
                        ui.set_width(box_w - 36.0);
                        ui.label(
                            RichText::new(tr::lng_signup_title())
                                .size(16.5)
                                .strong()
                                .color(palette.window_bold_fg),
                        );
                        ui.add_space(10.0);

                        // Preset geometric color avatars
                        let presets: [(&'static str, [u8; 3]); 4] = [
                            ("Sky", [0x40, 0xA7, 0xE3]),
                            ("Emerald", [0x5B, 0xB8, 0x67]),
                            ("Coral", [0xE1, 0x70, 0x76]),
                            ("Amber", [0xED, 0xA8, 0x6C]),
                        ];
                        ui.horizontal(|ui| {
                            for (label, rgb) in presets {
                                let (r, resp) =
                                    ui.allocate_exact_size(Vec2::splat(40.0), Sense::click());
                                ui.painter().circle_filled(
                                    r.center(),
                                    18.0,
                                    Color32::from_rgb(rgb[0], rgb[1], rgb[2]),
                                );
                                if resp.hovered() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                }
                                if resp.clicked() {
                                    self.photo = SignupPhoto::PresetColor { label, rgb };
                                    self.photo_texture = None;
                                    close = true;
                                }
                            }
                        });

                        ui.add_space(12.0);
                        ui.add_sized(
                            Vec2::new(box_w - 36.0, 30.0),
                            TextEdit::singleline(&mut self.photo_path_draft)
                                .hint_text("Path to PNG/JPEG image..."),
                        );
                        if !self.photo_error.is_empty() {
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(&self.photo_error)
                                    .size(12.0)
                                    .color(palette.attention_fg),
                            );
                        }
                        ui.add_space(14.0);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if link_button(ui, &tr::lng_box_ok(), palette).clicked() {
                                let trimmed = self.photo_path_draft.trim().to_owned();
                                if trimmed.is_empty() {
                                    close = true;
                                } else if let Err(err) =
                                    self.load_photo_from_path(Path::new(&trimmed))
                                {
                                    self.photo_error = err;
                                } else {
                                    close = true;
                                }
                            }
                            ui.add_space(12.0);
                            if link_button(ui, &tr::lng_cancel(), palette).clicked() {
                                close = true;
                            }
                        });
                    });
            });

        if close {
            self.photo_picker_open = false;
        }
    }

    fn show_terms_modal(
        &mut self,
        ctx: &egui::Context,
        tos: &TermsOfService,
        palette: &Palette,
    ) -> Option<SignupStepAction> {
        let screen = ctx.content_rect();
        let mut emitted = None;

        egui::Area::new(Id::unique("intro_terms_backdrop"))
            .order(Order::Foreground)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let _ = ui.allocate_exact_size(screen.size(), Sense::click());
                ui.painter()
                    .rect_filled(screen, 0.0, palette.layer_backdrop);
            });

        let box_w = 380.0_f32.min(screen.width() - 32.0);
        let box_pos = Pos2::new(screen.center().x - box_w * 0.5, screen.center().y - 160.0);

        egui::Area::new(Id::unique("intro_terms_box"))
            .order(Order::Tooltip)
            .fixed_pos(box_pos)
            .show(ctx, |ui| {
                Frame::NONE
                    .fill(palette.window_bg)
                    .corner_radius(CornerRadius::same(8))
                    .stroke(Stroke::new(1.0, palette.divider))
                    .inner_margin(Margin::same(20))
                    .show(ui, |ui| {
                        ui.set_width(box_w - 40.0);
                        ui.label(
                            RichText::new(tr::lng_terms_signup_link())
                                .size(17.0)
                                .strong()
                                .color(palette.window_bold_fg),
                        );
                        ui.add_space(12.0);

                        match self.terms_modal {
                            TermsModalState::ShowingTerms { submit_on_agree } => {
                                ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                                    ui.label(
                                        RichText::new(&tos.text)
                                            .size(13.5)
                                            .color(palette.window_fg),
                                    );
                                });
                                ui.add_space(18.0);
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    let agree_lbl = if submit_on_agree {
                                        tr::lng_terms_agree()
                                    } else {
                                        tr::lng_box_ok()
                                    };
                                    if link_button(ui, &agree_lbl.to_uppercase(), palette).clicked()
                                    {
                                        self.terms_modal = TermsModalState::Closed;
                                        if submit_on_agree {
                                            emitted = Some(SignupStepAction::AcceptTerms);
                                        }
                                    }
                                    if submit_on_agree {
                                        ui.add_space(14.0);
                                        if link_button(
                                            ui,
                                            &tr::lng_terms_decline().to_uppercase(),
                                            palette,
                                        )
                                        .clicked()
                                        {
                                            self.terms_modal =
                                                TermsModalState::DeclinedSorry { submit_on_agree };
                                        }
                                    }
                                });
                            }
                            TermsModalState::DeclinedSorry { submit_on_agree } => {
                                ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                                    ui.label(
                                        RichText::new(tr::lng_terms_signup_sorry())
                                            .size(13.5)
                                            .color(palette.window_fg),
                                    );
                                });
                                ui.add_space(18.0);
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if link_button(
                                        ui,
                                        &tr::lng_intro_finish().to_uppercase(),
                                        palette,
                                    )
                                    .clicked()
                                    {
                                        self.terms_modal =
                                            TermsModalState::ShowingTerms { submit_on_agree };
                                    }
                                    ui.add_space(14.0);
                                    if link_button(
                                        ui,
                                        &tr::lng_terms_decline().to_uppercase(),
                                        palette,
                                    )
                                    .clicked()
                                    {
                                        self.terms_modal = TermsModalState::Closed;
                                    }
                                });
                            }
                            TermsModalState::Closed => {}
                        }
                    });
            });

        emitted
    }
}

fn signup_initials(first: &str, last: &str) -> String {
    let mut out = String::new();
    if let Some(c) = first.trim().chars().next() {
        out.extend(c.to_uppercase());
    }
    if let Some(c) = last.trim().chars().next() {
        out.extend(c.to_uppercase());
    }
    if out.is_empty() { "TG".to_owned() } else { out }
}
