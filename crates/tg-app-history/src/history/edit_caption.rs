//! Edit-Caption modal box (`EditCaptionBox`) for editing captions and optionally replacing
//! media, toggling caption-above-media (`invert_media`), or applying a media spoiler via
//! `messages.editMessage` with `media`.
//!
//! Port origin: `Telegram/SourceFiles/boxes/edit_caption_box.h`,
//! `Telegram/SourceFiles/boxes/edit_caption_box.cpp`, and
//! `Telegram/SourceFiles/api/api_editing.cpp`.

use egui::{
    Align2, Color32, CornerRadius, FontId, Id, Key, Rect, Sense, Stroke, TextEdit, Ui, Vec2,
};
use tg_text::TextWithEntities;

use crate::business::model::PremiumLimitsData;
use crate::history::backend::{HistoryMedia, HistoryMessage};
use crate::history::text_bridge::parse_compose_markdown;
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::types::{MsgId, PeerId};

pub use tg_app_core::core_types::dto::edit_caption::*;

/// Stateful Edit-Caption modal box (`EditCaptionBox` in `boxes/edit_caption_box.cpp`).
#[derive(Debug, Clone)]
pub struct EditCaptionBoxState {
    /// Target chat peer id.
    pub peer_id: PeerId,
    /// Target message id.
    pub msg_id: MsgId,
    /// Human-readable description of the existing media attachment.
    pub media_summary: String,
    /// Whether the existing media supports spoiler toggle (photos and videos).
    pub supports_spoiler: bool,
    /// Editable caption text (markdown supported on submit).
    pub caption: String,
    /// Render caption above media (`invert_media`).
    pub invert_media: bool,
    /// Media spoiler veil toggle.
    pub spoiler: bool,
    /// Optional replacement media file picked by the user.
    pub replacement_media: Option<ReplacementMediaFile>,
    /// Whether Telegram Premium caption limit (2048 vs 1024) applies.
    pub is_premium: bool,
    /// Validation or submission error banner.
    pub error: Option<String>,
}

impl EditCaptionBoxState {
    /// Constructs an [`EditCaptionBoxState`] prefilled from an existing media [`HistoryMessage`].
    #[must_use]
    pub fn from_message(peer_id: PeerId, msg: &HistoryMessage, is_premium: bool) -> Self {
        let (media_summary, supports_spoiler) = match &msg.media {
            Some(HistoryMedia::Photo(p)) => (
                format!("Photo ({}×{}) · {}", p.width, p.height, p.file_name),
                true,
            ),
            Some(HistoryMedia::Video(v)) => (
                format!(
                    "{} ({}×{}, {}s)",
                    if v.is_gif { "GIF" } else { "Video" },
                    v.width,
                    v.height,
                    v.duration_secs
                ),
                true,
            ),
            Some(HistoryMedia::Document(d)) => (format!("File · {}", d.file_name), false),
            Some(HistoryMedia::Voice(v)) => {
                (format!("Voice message ({}s)", v.duration_secs), false)
            }
            Some(HistoryMedia::Album(items)) => (format!("Album ({} items)", items.len()), true),
            _ => ("Media".to_owned(), false),
        };
        Self {
            peer_id,
            msg_id: msg.id,
            media_summary,
            supports_spoiler,
            caption: msg.text.text.clone(),
            invert_media: false,
            spoiler: false,
            replacement_media: None,
            is_premium,
            error: None,
        }
    }

    /// Maximum allowed caption length in Unicode characters (`1024` free / `2048` Premium).
    #[must_use]
    pub fn caption_limit(&self) -> usize {
        let (free_limit, premium_limit) = PremiumLimitsData::default().caption_length;
        if self.is_premium {
            premium_limit as usize
        } else {
            free_limit as usize
        }
    }

    /// Attaches a replacement file to upload when saving.
    pub fn set_replacement_media(&mut self, file: ReplacementMediaFile) {
        self.supports_spoiler =
            file.as_photo || file.mime_type.starts_with("video/") || file.mime_type == "image/gif";
        self.replacement_media = Some(file);
        self.error = None;
    }

    /// Validates the current state and builds an [`EditMediaCaptionRequest`].
    pub fn build_request(&mut self) -> Result<EditMediaCaptionRequest, String> {
        let trimmed = self.caption.trim();
        let len = trimmed.chars().count();
        let limit = self.caption_limit();
        if len > limit {
            let err = format!("Caption is too long ({len}/{limit} characters).");
            self.error = Some(err.clone());
            return Err(err);
        }
        self.error = None;
        Ok(EditMediaCaptionRequest {
            caption: if trimmed.is_empty() {
                TextWithEntities::new()
            } else {
                parse_compose_markdown(trimmed)
            },
            invert_media: self.invert_media,
            spoiler: self.supports_spoiler && self.spoiler,
            replacement_media: self.replacement_media.clone(),
        })
    }

    /// Renders the Edit-Caption modal centered over the history window.
    pub fn show(&mut self, ui: &mut Ui, colors: &HistoryColors) -> Option<EditCaptionAction> {
        let mut action = None;
        let full = ui.ctx().content_rect();
        let mut clicked_outside = false;
        let mut card_rect = Rect::NOTHING;

        egui::Area::new(Id::unique("tg_edit_caption_scrim"))
            .order(egui::Order::Middle)
            .fixed_pos(full.min)
            .show(ui.ctx(), |ui| {
                let (r, resp) = ui.allocate_exact_size(full.size(), Sense::click());
                ui.painter().rect_filled(
                    r,
                    CornerRadius::ZERO,
                    Color32::from_rgba_unmultiplied(0, 0, 0, 110),
                );
                if resp.clicked() {
                    clicked_outside = true;
                }
            });

        egui::Area::new(Id::unique("tg_edit_caption_card"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                let resp = egui::Frame::NONE
                    .fill(colors.bar_bg)
                    .stroke(Stroke::new(1.0, colors.divider))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(egui::Margin::same(18))
                    .show(ui, |ui| {
                        ui.set_width(380.0);
                        ui.label(
                            egui::RichText::new("Edit Media & Caption")
                                .strong()
                                .size(16.0)
                                .color(colors.bold_fg),
                        );
                        ui.add_space(8.0);

                        // Media summary & Replace button
                        egui::Frame::NONE
                            .fill(colors.bar_bg_over)
                            .corner_radius(CornerRadius::same(8))
                            .inner_margin(egui::Margin::symmetric(10, 8))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    let active_label = match &self.replacement_media {
                                        Some(rep) => format!(
                                            "Replacement: {} ({} bytes)",
                                            rep.file_name,
                                            rep.bytes.len()
                                        ),
                                        None => self.media_summary.clone(),
                                    };
                                    ui.label(
                                        egui::RichText::new(active_label)
                                            .size(12.5)
                                            .color(colors.bold_fg),
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if self.replacement_media.is_some()
                                                && ui.small_button("Revert").clicked()
                                            {
                                                self.replacement_media = None;
                                            }
                                            if ui.small_button("Replace...").clicked()
                                                && let Some(path) = rfd::FileDialog::new()
                                                    .set_title("Replace media")
                                                    .pick_file()
                                            {
                                                match std::fs::read(&path) {
                                                    Ok(bytes) => {
                                                        let name = path
                                                            .file_name()
                                                            .and_then(|n| n.to_str())
                                                            .unwrap_or("replacement.bin")
                                                            .to_owned();
                                                        self.set_replacement_media(
                                                            ReplacementMediaFile::from_bytes(
                                                                name, bytes,
                                                            ),
                                                        );
                                                    }
                                                    Err(err) => {
                                                        self.error = Some(format!(
                                                            "Failed to read file: {err}"
                                                        ));
                                                    }
                                                }
                                            }
                                        },
                                    );
                                });
                            });

                        ui.add_space(8.0);

                        // Caption options
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut self.invert_media, "Move caption above media");
                            if self.supports_spoiler {
                                ui.add_space(8.0);
                                ui.checkbox(&mut self.spoiler, "Hide with spoiler");
                            }
                        });

                        ui.add_space(8.0);

                        let limit = self.caption_limit();
                        let used = self.caption.trim().chars().count();
                        let rem = limit as isize - used as isize;
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("Caption")
                                    .strong()
                                    .size(12.5)
                                    .color(colors.active_text_fg),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        egui::RichText::new(format!("{used}/{limit}"))
                                            .font(FontId::proportional(11.0))
                                            .color(if rem < 0 {
                                                colors.attention_fg
                                            } else {
                                                colors.sub_text_fg
                                            }),
                                    );
                                },
                            );
                        });
                        ui.add(
                            TextEdit::multiline(&mut self.caption)
                                .desired_width(f32::INFINITY)
                                .desired_rows(4)
                                .hint_text("Add a caption..."),
                        );

                        if let Some(ref err) = self.error {
                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new(err)
                                    .size(12.0)
                                    .color(colors.attention_fg),
                            );
                        }

                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                egui::RichText::new("Save")
                                                    .color(Color32::WHITE)
                                                    .strong(),
                                            )
                                            .fill(colors.accent)
                                            .corner_radius(CornerRadius::same(6)),
                                        )
                                        .clicked()
                                        && let Ok(request) = self.build_request()
                                    {
                                        action = Some(EditCaptionAction::Submit {
                                            peer_id: self.peer_id,
                                            msg_id: self.msg_id,
                                            request,
                                        });
                                    }
                                    ui.add_space(8.0);
                                    if ui.button("Cancel").clicked() {
                                        action = Some(EditCaptionAction::Cancelled);
                                    }
                                },
                            );
                        });
                    });
                card_rect = resp.response.rect;
            });

        if action.is_none() {
            let escaped = ui.ctx().input(|i| i.key_pressed(Key::Escape));
            let outside = clicked_outside
                && ui
                    .ctx()
                    .pointer_interact_pos()
                    .is_none_or(|p| !card_rect.contains(p));
            if escaped || outside {
                action = Some(EditCaptionAction::Cancelled);
            }
        }

        action
    }
}
