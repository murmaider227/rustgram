//! Sticker & Custom Emoji Creator modal (`StickerCreatorBox`) and backend contract.
//!
//! Port origin:
//! - `Telegram/SourceFiles/boxes/sticker_creator_box.cpp`
//! - `Telegram/SourceFiles/api/api_stickers_creator.cpp`

use std::path::Path;

use egui::{Color32, CornerRadius, ScrollArea, TextEdit, Ui};
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::Colors;

trait StickerCreatorColorsExt {
    fn window_bold_fg(&self) -> Color32;
    fn window_bg_over(&self) -> Color32;
    fn window_fg(&self) -> Color32;
    fn window_sub_text_fg(&self) -> Color32;
    fn attention_fg(&self) -> Color32;
}

impl<T: Colors + ?Sized> StickerCreatorColorsExt for T {
    fn window_bold_fg(&self) -> Color32 {
        self.color("windowBoldFg")
    }
    fn window_bg_over(&self) -> Color32 {
        self.color("windowBgOver")
    }
    fn window_fg(&self) -> Color32 {
        self.color("windowFg")
    }
    fn window_sub_text_fg(&self) -> Color32 {
        self.color("windowSubTextFg")
    }
    fn attention_fg(&self) -> Color32 {
        self.color("attentionButtonFg")
    }
}

use crate::boxes::compose_ai::{paint_accent_button, show_modal_card};

// ---------------------------------------------------------------------------
// Constants (matching `api_stickers_creator.cpp` & `sticker_creator_box.cpp`)
// ---------------------------------------------------------------------------

pub use tg_app_core::core_types::dto::sticker_creator::*;

/// Destination mode in [`StickerCreatorBoxState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StickerTargetMode {
    /// Add sticker to an existing owned pack (`stickers.addStickerToSet`).
    #[default]
    AddToExisting,
    /// Create a brand-new pack (`stickers.createStickerSet`).
    CreateNewPack,
}

/// Outcome emitted by [`StickerCreatorBoxState::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StickerCreatorAction {
    /// Sticker set was created or updated.
    Saved(OwnedStickerSetSummary),
    /// User cancelled.
    Cancelled,
}

/// Stateful Sticker & Custom Emoji Creator modal (`StickerCreatorBox`).
#[derive(Debug, Clone)]
pub struct StickerCreatorBoxState {
    /// Regular sticker vs Custom Emoji mode.
    pub kind: StickerSetKind,
    /// Add to existing pack vs Create new pack.
    pub target_mode: StickerTargetMode,
    /// Prepared sticker asset (defaults to a generated sample artwork until user picks a file).
    pub asset: Option<PreparedStickerAsset>,
    /// Source file label displayed in the UI.
    pub source_label: String,
    /// Associated emojis (up to 7).
    pub emoji: String,
    /// Optional search keywords.
    pub keywords: String,
    /// Loaded owned sticker sets for `self.kind`.
    pub owned_sets: Vec<OwnedStickerSetSummary>,
    /// Selected index in `owned_sets`.
    pub selected_set_index: usize,
    /// New pack title (`CreateNewPack` mode).
    pub new_title: String,
    /// New pack short name (`CreateNewPack` mode).
    pub new_short_name: String,
    /// Cached availability result for `new_short_name`.
    pub short_name_available: Option<bool>,
    /// Inline error or validation message.
    pub error: Option<String>,
    /// Whether owned sets have been fetched for current `kind`.
    pub sets_loaded: bool,
}

impl Default for StickerCreatorBoxState {
    fn default() -> Self {
        Self::new(StickerSetKind::Regular)
    }
}

impl StickerCreatorBoxState {
    /// Opens a `StickerCreatorBox` for `kind` pre-seeded with a valid procedural sample image.
    #[must_use]
    pub fn new(kind: StickerSetKind) -> Self {
        let sample = generate_sample_sticker_rgba(3, kind.max_side());
        let asset = prepare_static_sticker_image(&sample, kind).ok();
        Self {
            kind,
            target_mode: StickerTargetMode::AddToExisting,
            asset,
            source_label: "sample_badge.webp".to_owned(),
            emoji: "✨".to_owned(),
            keywords: String::new(),
            owned_sets: Vec::new(),
            selected_set_index: 0,
            new_title: String::new(),
            new_short_name: String::new(),
            short_name_available: None,
            error: None,
            sets_loaded: false,
        }
    }

    /// Loads an image or WebM file from disk into the creator.
    pub fn load_file(&mut self, path: &Path) -> Result<(), String> {
        let prepared = prepare_sticker_from_file(path, self.kind)?;
        self.source_label = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("sticker")
            .to_owned();
        self.asset = Some(prepared);
        self.error = None;
        Ok(())
    }

    /// Loads owned sets from `backend` when `sets_loaded` is false.
    pub fn ensure_sets_loaded(&mut self, backend: &mut dyn StickerCreatorBackend) {
        if self.sets_loaded {
            return;
        }
        self.owned_sets = backend.owned_sticker_sets(self.kind);
        self.selected_set_index = 0;
        if self.owned_sets.is_empty() {
            self.target_mode = StickerTargetMode::CreateNewPack;
        }
        self.sets_loaded = true;
    }

    /// Submits either `stickers.createStickerSet` or `stickers.addStickerToSet` through `backend`.
    pub fn submit(
        &mut self,
        backend: &mut dyn StickerCreatorBackend,
    ) -> Result<OwnedStickerSetSummary, String> {
        self.ensure_sets_loaded(backend);
        let Some(asset) = self.asset.clone() else {
            let msg = "Please choose an image or WebM file first.".to_owned();
            self.error = Some(msg.clone());
            return Err(msg);
        };
        let emoji = normalize_sticker_emoji(&self.emoji);
        let res = match self.target_mode {
            StickerTargetMode::CreateNewPack => {
                if self.new_short_name.trim().is_empty()
                    && !self.new_title.trim().is_empty()
                    && let Ok(suggested) = backend.suggest_sticker_short_name(&self.new_title)
                {
                    self.new_short_name = suggested;
                }
                backend.create_sticker_set(CreateStickerSetRequest {
                    title: self.new_title.trim().to_owned(),
                    short_name: self.new_short_name.trim().to_owned(),
                    kind: self.kind,
                    asset,
                    emoji,
                    keywords: self.keywords.trim().to_owned(),
                })
            }
            StickerTargetMode::AddToExisting => {
                let Some(target) = self.owned_sets.get(self.selected_set_index) else {
                    let msg = "Select an existing pack or switch to Create New Pack.".to_owned();
                    self.error = Some(msg.clone());
                    return Err(msg);
                };
                backend.add_sticker_to_set(AddStickerToSetRequest {
                    set_id: target.id,
                    set_access_hash: target.access_hash,
                    set_short_name: target.short_name.clone(),
                    kind: self.kind,
                    asset,
                    emoji,
                    keywords: self.keywords.trim().to_owned(),
                })
            }
        };
        match res {
            Ok(summary) => {
                self.error = None;
                self.sets_loaded = false;
                Ok(summary)
            }
            Err(err) => {
                self.error = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Builds a `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        BoxModal::new(id, "Sticker Creator")
            .width(430.0)
            .add_button(BoxButton::default_btn("create", "Save Sticker"))
            .add_button(BoxButton::default_btn("cancel", "Cancel"))
    }

    /// Renders the `StickerCreatorBox` modal.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn StickerCreatorBackend,
        colors: &dyn Colors,
    ) -> Option<StickerCreatorAction> {
        self.ensure_sets_loaded(backend);
        let mut action = None;

        show_modal_card(ui, "tg_sticker_creator_modal", 430.0, colors, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Sticker Creator")
                        .strong()
                        .size(16.0)
                        .color(colors.window_bold_fg()),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if tg_app_core::ui_icons::small_icon_button(
                        ui,
                        "Close",
                        true,
                        tg_app_core::ui_icons::paint_close_cross,
                    )
                    .clicked()
                    {
                        action = Some(StickerCreatorAction::Cancelled);
                    }
                });
            });
            ui.add_space(6.0);

            // Sticker vs Custom Emoji selector
            ui.horizontal(|ui| {
                for (k, label) in [
                    (StickerSetKind::Regular, "Sticker (512px)"),
                    (StickerSetKind::CustomEmoji, "Custom Emoji (100px)"),
                ] {
                    if ui.selectable_label(self.kind == k, label).clicked() && self.kind != k {
                        self.kind = k;
                        self.sets_loaded = false;
                        let sample = generate_sample_sticker_rgba(5, k.max_side());
                        self.asset = prepare_static_sticker_image(&sample, k).ok();
                        self.ensure_sets_loaded(backend);
                    }
                }
            });
            ui.add_space(8.0);

            // Asset summary & file picker
            egui::Frame::NONE
                .fill(colors.window_bg_over())
                .corner_radius(CornerRadius::same(8))
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let meta = if let Some(ref a) = self.asset {
                            format!(
                                "{} · {}×{} · {} KB ({})",
                                self.source_label,
                                a.width,
                                a.height,
                                (a.bytes.len() / 1024).max(1),
                                a.mime_type
                            )
                        } else {
                            "No sticker image selected".to_owned()
                        };
                        ui.label(
                            egui::RichText::new(meta)
                                .size(12.0)
                                .color(colors.window_fg()),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Choose File...").clicked()
                                && let Some(path) = rfd::FileDialog::new()
                                    .set_title("Choose Sticker Image or WebM")
                                    .add_filter(
                                        "Sticker Media",
                                        &["png", "jpg", "jpeg", "webp", "webm"],
                                    )
                                    .pick_file()
                                && let Err(err) = self.load_file(&path)
                            {
                                self.error = Some(err);
                            }
                        });
                    });
                });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Emoji:")
                        .size(12.0)
                        .color(colors.window_sub_text_fg()),
                );
                ui.add(
                    TextEdit::singleline(&mut self.emoji)
                        .desired_width(90.0)
                        .hint_text("🙂"),
                );
                for preset in ["✨", "🔥", "❤️", "😂", "👍"] {
                    if ui.small_button(preset).clicked()
                        && self.emoji.chars().count() < MAX_STICKER_EMOJIS
                    {
                        self.emoji.push_str(preset);
                    }
                }
            });
            ui.add_space(4.0);
            ui.add(
                TextEdit::singleline(&mut self.keywords)
                    .desired_width(f32::INFINITY)
                    .hint_text("Search keywords (optional, comma-separated)"),
            );

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(
                        self.target_mode == StickerTargetMode::AddToExisting,
                        "Add to Owned Pack",
                    )
                    .clicked()
                {
                    self.target_mode = StickerTargetMode::AddToExisting;
                }
                if ui
                    .selectable_label(
                        self.target_mode == StickerTargetMode::CreateNewPack,
                        "Create New Pack",
                    )
                    .clicked()
                {
                    self.target_mode = StickerTargetMode::CreateNewPack;
                }
            });
            ui.add_space(6.0);

            match self.target_mode {
                StickerTargetMode::AddToExisting => {
                    if self.owned_sets.is_empty() {
                        ui.label(
                            egui::RichText::new(
                                "You do not own any packs of this type yet. Switch to Create New Pack.",
                            )
                            .size(12.0)
                            .color(colors.window_sub_text_fg()),
                        );
                    } else {
                        ScrollArea::vertical()
                            .id_salt("tg_sticker_creator_sets_scroll")
                            .max_height(110.0)
                            .show(ui, |ui| {
                                for (idx, set) in self.owned_sets.iter().enumerate() {
                                    let label = format!(
                                        "{} ({}/{}) — @{}",
                                        set.title,
                                        set.count,
                                        set.kind.max_items(),
                                        set.short_name
                                    );
                                    if ui.radio(self.selected_set_index == idx, label).clicked() {
                                        self.selected_set_index = idx;
                                        self.error = None;
                                    }
                                }
                            });
                    }
                }
                StickerTargetMode::CreateNewPack => {
                    ui.add(
                        TextEdit::singleline(&mut self.new_title)
                            .desired_width(f32::INFINITY)
                            .hint_text("Pack title (e.g. My Custom Pack)"),
                    );
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.add(
                            TextEdit::singleline(&mut self.new_short_name)
                                .desired_width(250.0)
                                .hint_text("Short link name (e.g. my_custom_pack)"),
                        );
                        if ui.small_button("Suggest").clicked() {
                            match backend.suggest_sticker_short_name(&self.new_title) {
                                Ok(s) => {
                                    self.new_short_name = s;
                                    self.short_name_available = Some(true);
                                    self.error = None;
                                }
                                Err(err) => self.error = Some(err),
                            }
                        }
                        if ui.small_button("Check").clicked() {
                            match backend.check_sticker_short_name(&self.new_short_name) {
                                Ok(avail) => {
                                    self.short_name_available = Some(avail);
                                    self.error = if avail {
                                        None
                                    } else {
                                        Some("Short name is already taken.".to_owned())
                                    };
                                }
                                Err(err) => {
                                    self.short_name_available = Some(false);
                                    self.error = Some(err);
                                }
                            }
                        }
                    });
                    if let Some(avail) = self.short_name_available {
                        let status_color = if avail {
                            egui::Color32::from_rgb(0x2E, 0x9E, 0x5B)
                        } else {
                            colors.attention_fg()
                        };
                        tg_app_core::ui_icons::inline_icon_label(
                            ui,
                            13.0,
                            |p, c, s| {
                                if avail {
                                    tg_app_core::ui_icons::paint_select_check(
                                        p,
                                        c,
                                        s,
                                        status_color,
                                    );
                                } else {
                                    tg_app_core::ui_icons::paint_close_cross(p, c, s, status_color);
                                }
                            },
                            egui::RichText::new(if avail {
                                "Short name is available"
                            } else {
                                "Short name is unavailable"
                            })
                            .size(11.5)
                            .color(status_color),
                        );
                    }
                }
            }

            if let Some(ref err) = self.error {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(err)
                        .size(12.0)
                        .color(colors.attention_fg()),
                );
            }

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let submit_label = match self.target_mode {
                        StickerTargetMode::AddToExisting => "Add Sticker",
                        StickerTargetMode::CreateNewPack => "Create Pack",
                    };
                    if paint_accent_button(ui, submit_label, colors)
                        && let Ok(summary) = self.submit(backend)
                    {
                        action = Some(StickerCreatorAction::Saved(summary));
                    }
                    ui.add_space(8.0);
                    if ui.button("Cancel").clicked() {
                        action = Some(StickerCreatorAction::Cancelled);
                    }
                });
            });
        });

        if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
            action = Some(StickerCreatorAction::Cancelled);
        }
        action
    }
}
