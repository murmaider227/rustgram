//! Chat background gallery picker and interactive message preview box.
//!
//! Port of `Telegram/SourceFiles/boxes/background_box.h`, `background_box.cpp`,
//! `Telegram/SourceFiles/boxes/background_preview_box.h`, and `background_preview_box.cpp`.

use std::io::Cursor;
use std::path::Path;

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Margin, Pos2, Rect, Role, ScrollArea,
    Sense, Stroke, StrokeKind, Ui, Vec2, WidgetInfo,
};
use image::ImageFormat;
use tg_lang::tr;
use tg_theme::{Color, WallPaper, WallPaperFlags, prepare_base_palette};
use tg_ui::theme::{Colors, theme_color_to_egui};

use crate::themes::backend::{CloudWallPaperRecord, ThemesBackend};
use crate::themes::chat_background::{BackgroundConfig, ChatBackground, PatternKind};
use crate::themes::color_editor::{ColorEditorMode, ColorEditorState};

/// Target scope when applying a background from [`BackgroundPreviewState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackgroundApplyScope {
    /// Apply as the global default background for all chats (`applyForEveryone`).
    #[default]
    AllChats,
    /// Apply only for a single chat (`applyForPeer`).
    SingleChat {
        /// Target peer ID.
        peer_id: i64,
        /// Placeholder flag mirroring Telegram Premium "Apply for both" option.
        for_both: bool,
    },
}

/// Filter category in the background gallery (`BackgroundBox`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackgroundCategory {
    /// Show all gallery entries.
    #[default]
    All,
    /// Multi-color freeform & linear gradients.
    Gradients,
    /// Patterned backgrounds with adjustable intensity.
    Patterns,
    /// Solid single-color wallpapers.
    Solid,
    /// Custom user-loaded image wallpapers.
    Custom,
}

impl BackgroundCategory {
    /// Ordered category tabs.
    pub const ALL: &'static [Self] = &[
        Self::All,
        Self::Gradients,
        Self::Patterns,
        Self::Solid,
        Self::Custom,
    ];

    /// Human-readable tab label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Gradients => "Gradients",
            Self::Patterns => "Patterns",
            Self::Solid => "Solid",
            Self::Custom => "From File",
        }
    }
}

/// One selectable item inside [`BackgroundPickerState`].
#[derive(Debug, Clone)]
pub struct BackgroundGalleryItem {
    /// Stable unique identifier.
    pub id: String,
    /// Human-readable display title.
    pub name: String,
    /// Category tag for filtering.
    pub category: BackgroundCategory,
    /// Full background configuration.
    pub config: BackgroundConfig,
    /// Whether the user can remove this entry (custom or saved cloud wallpapers).
    pub removable: bool,
    /// Associated cloud wallpaper metadata when loaded from `account.getWallPapers` / `uploadWallPaper`.
    pub cloud_record: Option<CloudWallPaperRecord>,
}

/// Interactive preview state (`BackgroundPreviewBox`) with sample messages, Day/Night preview
/// switch, Blur, Motion, Tiled, Pattern Intensity, Rotation, Dark Dimming, and apply scope.
pub struct BackgroundPreviewState {
    /// Live working copy of the background being previewed.
    pub config: BackgroundConfig,
    /// Independent dark/light preview mode (`_boxDarkMode` in `background_preview_box.cpp`).
    pub preview_dark_mode: bool,
    /// Optional target chat when previewing for a single chat (`_forPeer`).
    pub target_peer: Option<(i64, String)>,
    /// Whether "Apply for both" is checked for 1:1 chats (`messages.setChatWallPaper.for_both`).
    pub apply_for_both: bool,
    /// Associated cloud wallpaper record for the active previewed item, if any.
    pub cloud_record: Option<CloudWallPaperRecord>,
    /// Whether live cloud wallpaper sync is connected.
    pub cloud_synced: bool,
    /// Inline solid color editor when editing a solid color background.
    solid_editor: ColorEditorState,
    /// Whether the inline solid color picker is expanded.
    pub solid_editor_open: bool,
}

impl BackgroundPreviewState {
    /// Creates a preview state for `config` with optional target peer `(peer_id, peer_name)`.
    #[must_use]
    pub fn new(
        config: BackgroundConfig,
        dark_mode: bool,
        target_peer: Option<(i64, String)>,
    ) -> Self {
        let initial_color = config
            .wallpaper
            .background_colors
            .first()
            .copied()
            .unwrap_or(Color::rgb(0xD5, 0xE2, 0xEC));
        Self {
            config,
            preview_dark_mode: dark_mode,
            target_peer,
            apply_for_both: false,
            cloud_record: None,
            cloud_synced: false,
            solid_editor: ColorEditorState::new(ColorEditorMode::Rgba, initial_color),
            solid_editor_open: false,
        }
    }

    /// Updates the previewed configuration and synchronizes the solid color editor.
    pub fn set_config(&mut self, config: BackgroundConfig) {
        self.set_config_with_cloud(config, None);
    }

    /// Updates the previewed configuration along with its optional [`CloudWallPaperRecord`].
    pub fn set_config_with_cloud(
        &mut self,
        config: BackgroundConfig,
        cloud_record: Option<CloudWallPaperRecord>,
    ) {
        if let Some(&first) = config.wallpaper.background_colors.first() {
            self.solid_editor = ColorEditorState::new(ColorEditorMode::Rgba, first);
        }
        self.config = config;
        self.cloud_record = cloud_record;
    }

    /// Builds a [`CloudWallPaperRecord`] reflecting the current previewed wallpaper,
    /// including updated pattern, intensity, blur, motion, rotation, and colors.
    #[must_use]
    pub fn to_cloud_record(&self) -> CloudWallPaperRecord {
        let mut rec = self
            .cloud_record
            .clone()
            .unwrap_or_else(|| CloudWallPaperRecord {
                wallpaper: self.config.wallpaper.clone(),
                motion: self.config.motion,
                is_no_file: self.config.custom_image.is_none()
                    && !self.config.wallpaper.is_pattern()
                    && self.config.wallpaper.document_id.is_none(),
                document: None,
            });
        rec.wallpaper = self.config.wallpaper.clone();
        rec.motion = self.config.motion;
        rec
    }

    /// Encodes the previewed custom photo background as JPEG bytes for `account.uploadWallPaper`.
    #[must_use]
    pub fn encode_custom_image_jpeg(&self) -> Option<Vec<u8>> {
        let img = self.config.custom_image.as_ref()?;
        let rgb = image::DynamicImage::ImageRgba8((**img).clone()).into_rgb8();
        let mut buf = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(rgb)
            .write_to(&mut buf, ImageFormat::Jpeg)
            .ok()?;
        Some(buf.into_inner())
    }

    /// Sets pattern intensity (`-100..=100`), updating the underlying `WallPaper`.
    pub fn set_pattern_intensity(&mut self, intensity: i32) {
        self.config.wallpaper = self
            .config
            .wallpaper
            .with_pattern_intensity(intensity.clamp(-100, 100));
    }

    /// Sets pattern mask kind (automatically toggling `WallPaperFlags::PATTERN` and clearing
    /// any attached vector `.tgv`/SVG pattern bytes when switching to a procedural mask or None).
    pub fn set_pattern_kind(&mut self, kind: PatternKind) {
        self.config.pattern = kind;
        self.config.pattern_bytes = None;
        let bits = self.config.wallpaper.flags.bits();
        self.config.wallpaper.flags = if kind == PatternKind::None {
            WallPaperFlags::from_bits(bits & !WallPaperFlags::PATTERN.bits())
        } else {
            WallPaperFlags::from_bits(bits | WallPaperFlags::PATTERN.bits())
        };
    }

    /// Attaches raw SVG or gzipped `.tgv` vector pattern bytes directly to the preview config.
    pub fn set_svg_pattern_bytes(&mut self, bytes: Vec<u8>) {
        self.config = self.config.clone().with_pattern_bytes(bytes);
    }

    /// Sets linear gradient rotation quantized to multiples of 45 degrees (`0..=315`).
    pub fn set_rotation(&mut self, degrees: i32) {
        self.config.wallpaper.rotation = (degrees.rem_euclid(360) / 45) * 45;
    }

    /// Toggles Gaussian/StackBlur on image backgrounds (`_blur`).
    pub fn set_blurred(&mut self, blurred: bool) {
        self.config.wallpaper = self.config.wallpaper.with_blurred(blurred);
    }

    /// Toggles smooth complex-gradient motion animation.
    pub fn set_motion(&mut self, motion: bool) {
        self.config.motion = motion;
    }

    /// Computes the shareable `https://t.me/bg/...` URL if the wallpaper has a slug.
    #[must_use]
    pub fn share_url(&self) -> Option<String> {
        self.config.wallpaper.share_url("https://t.me")
    }

    /// Applies the previewed background onto `chat_bg` for `scope`.
    pub fn apply_to(&self, chat_bg: &mut ChatBackground, scope: BackgroundApplyScope) {
        match scope {
            BackgroundApplyScope::AllChats => {
                chat_bg.set_global(self.config.clone());
            }
            BackgroundApplyScope::SingleChat { peer_id, .. } => {
                chat_bg.set_peer(peer_id, self.config.clone());
            }
        }
    }

    /// Renders the interactive background preview card with sample messages and controls.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        chat_bg: &mut ChatBackground,
        colors: &dyn Colors,
    ) -> BackgroundPreviewOutput {
        let mut out = BackgroundPreviewOutput::default();
        let bold_fg = colors.color("windowBoldFg");
        let sub_fg = colors.color("windowSubTextFg");
        let divider = colors.color("shadowFg");
        let accent_bg = colors.color("activeButtonBg");
        let accent_fg = colors.color("activeButtonFg");

        // Header: title + Day/Night preview switch + Share link
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(tr::lng_background_header())
                    .size(15.0)
                    .strong()
                    .color(bold_fg),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mode_label = if self.preview_dark_mode {
                    "Preview: Night"
                } else {
                    "Preview: Day"
                };
                if ui.button(mode_label).clicked() {
                    self.preview_dark_mode = !self.preview_dark_mode;
                }
                if let Some(url) = self.share_url()
                    && ui.button(tr::lng_background_share()).clicked()
                {
                    ui.ctx().copy_text(url);
                    out.toast = Some(tr::lng_background_link_copied());
                }
            });
        });

        ui.add_space(6.0);

        // Live preview canvas with sample messages (`BackgroundPreviewBox::paintEvent`)
        let canvas_w = ui.available_width().max(240.0);
        let canvas_h = 232.0;
        let (canvas_rect, _) =
            ui.allocate_exact_size(Vec2::new(canvas_w, canvas_h), Sense::hover());

        chat_bg.paint_config(ui, canvas_rect, &self.config, self.preview_dark_mode);
        paint_sample_messages(
            ui,
            canvas_rect,
            self.preview_dark_mode,
            self.target_peer.is_some(),
        );
        ui.painter().rect_stroke(
            canvas_rect,
            CornerRadius::same(10),
            Stroke::new(1.0, divider),
            StrokeKind::Inside,
        );

        ui.add_space(8.0);

        // Controls: Blur, Motion, Tiled, Pattern selector, Intensity, Rotation, Dimming
        ui.horizontal_wrapped(|ui| {
            let mut blurred = self.config.wallpaper.blurred;
            if ui
                .add_enabled(
                    self.config.supports_blur(),
                    egui::Checkbox::new(&mut blurred, tr::lng_background_blur()),
                )
                .changed()
            {
                self.set_blurred(blurred);
            }

            let mut motion = self.config.motion;
            if ui
                .add_enabled(
                    self.config.supports_motion(),
                    egui::Checkbox::new(&mut motion, "Motion"),
                )
                .changed()
            {
                self.set_motion(motion);
            }

            if self.config.motion
                && self.config.supports_motion()
                && ui.small_button("Animate Step").clicked()
            {
                chat_bg.trigger_motion_step();
            }

            ui.checkbox(&mut self.config.tiled, tr::lng_settings_bg_tile());

            if self.config.custom_image.is_none()
                && self.config.wallpaper.background_colors.len() == 1
                && ui.small_button("Pick Solid Color").clicked()
            {
                self.solid_editor_open = !self.solid_editor_open;
            }
        });

        // Pattern style + intensity slider
        if self.config.custom_image.is_none() {
            ui.add_space(4.0);
            if self.config.has_svg_pattern() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new("Pattern: SVG / .tgv vector mask")
                            .size(12.0)
                            .strong()
                            .color(bold_fg),
                    );
                    if ui.small_button("Switch to Built-in").clicked() {
                        self.set_pattern_kind(PatternKind::Doodles);
                    }
                });
            } else {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("Pattern:").size(12.0).color(sub_fg));
                    let kinds = [
                        PatternKind::None,
                        PatternKind::Doodles,
                        PatternKind::Waves,
                        PatternKind::Hexagons,
                        PatternKind::Botanical,
                        PatternKind::Constellation,
                    ];
                    for kind in kinds {
                        if ui
                            .selectable_label(self.config.pattern == kind, kind.label())
                            .clicked()
                        {
                            self.set_pattern_kind(kind);
                        }
                    }
                });
            }

            if self.config.pattern != PatternKind::None || self.config.has_svg_pattern() {
                let mut intensity = self.config.wallpaper.intensity;
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Intensity:").size(12.0).color(sub_fg));
                    if ui
                        .add(egui::Slider::new(&mut intensity, -100..=100).suffix("%"))
                        .changed()
                    {
                        self.set_pattern_intensity(intensity);
                    }
                });
            }

            // Linear gradient rotation selector (for 2-color linear gradients)
            if self.config.wallpaper.background_colors.len() == 2 {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("Angle:").size(12.0).color(sub_fg));
                    for deg in [0, 45, 90, 135, 180, 225, 270, 315] {
                        if ui
                            .selectable_label(
                                self.config.wallpaper.rotation == deg,
                                format!("{deg}°"),
                            )
                            .clicked()
                        {
                            self.set_rotation(deg);
                        }
                    }
                });
            }
        }

        // Dark dimming slider for photo backgrounds in dark preview mode
        if self.config.custom_image.is_some() && self.preview_dark_mode {
            let mut dim = i32::from(self.config.dark_dimming);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(tr::lng_background_dimming())
                        .size(12.0)
                        .color(sub_fg),
                );
                if ui
                    .add(egui::Slider::new(&mut dim, 0..=100).suffix("%"))
                    .changed()
                {
                    self.config.dark_dimming = dim.clamp(0, 100) as u8;
                }
            });
        }

        // Optional inline solid color editor
        if self.solid_editor_open {
            ui.add_space(6.0);
            if self.solid_editor.show(ui, colors) {
                let c = self.solid_editor.color().with_alpha(255);
                self.config.wallpaper = self.config.wallpaper.with_background_colors(vec![c]);
            }
        }

        ui.add_space(10.0);

        // Apply actions: All Chats vs Single Chat
        ui.horizontal_wrapped(|ui| {
            let apply_all = egui::Button::new(
                egui::RichText::new("Apply for All Chats")
                    .color(accent_fg)
                    .strong(),
            )
            .fill(accent_bg)
            .min_size(Vec2::new(146.0, 32.0));
            if ui.add(apply_all).clicked() {
                self.apply_to(chat_bg, BackgroundApplyScope::AllChats);
                out.applied = Some(BackgroundApplyScope::AllChats);
                out.toast = Some("Background applied to all chats".to_owned());
            }

            if let Some((peer_id, ref peer_name)) = self.target_peer {
                let label = format!("Apply for {peer_name}");
                if ui
                    .add(egui::Button::new(label).min_size(Vec2::new(140.0, 32.0)))
                    .clicked()
                {
                    let scope = BackgroundApplyScope::SingleChat {
                        peer_id,
                        for_both: self.apply_for_both,
                    };
                    self.apply_to(chat_bg, scope);
                    out.applied = Some(scope);
                    out.toast = Some(if self.apply_for_both {
                        if self.cloud_synced {
                            format!("Background applied for {peer_name} (for both)")
                        } else {
                            format!(
                                "Background applied for {peer_name} (sync for both requires MTProto)"
                            )
                        }
                    } else {
                        format!("Background applied for {peer_name}")
                    });
                }
                ui.checkbox(&mut self.apply_for_both, "Set for both");
                if ui.small_button("Reset Chat Wallpaper").clicked() {
                    chat_bg.clear_peer(peer_id);
                    out.reset_peer_requested = Some(peer_id);
                    out.toast = Some(format!("Reset wallpaper for {peer_name}"));
                }
            }

            if self.cloud_synced && ui.small_button("Save to Cloud").clicked() {
                out.save_cloud_requested = true;
            }
        });

        out
    }
}

/// Output emitted by [`BackgroundPreviewState::show`].
#[derive(Debug, Clone, Default)]
pub struct BackgroundPreviewOutput {
    /// Non-`None` when the user clicked an Apply button.
    pub applied: Option<BackgroundApplyScope>,
    /// User clicked "Save to Cloud" (`account.saveWallPaper`).
    pub save_cloud_requested: bool,
    /// User clicked "Reset Chat Wallpaper" (`messages.setChatWallPaper(revert: true)`).
    pub reset_peer_requested: Option<i64>,
    /// Transient toast notification.
    pub toast: Option<String>,
}

/// Output emitted by [`BackgroundPickerState::show`].
#[derive(Debug, Clone, Default)]
pub struct BackgroundPickerOutput {
    /// Non-`None` when the user applied a background.
    pub applied: Option<BackgroundApplyScope>,
    /// User clicked "Save to Cloud" (`account.saveWallPaper(unsave: false)`).
    pub save_cloud_requested: bool,
    /// User clicked "Reset Chat Wallpaper" for `peer_id`.
    pub reset_peer_requested: Option<i64>,
    /// User deleted a saved cloud wallpaper (`account.saveWallPaper(unsave: true)`).
    pub removed_cloud: Option<CloudWallPaperRecord>,
    /// Transient toast notification.
    pub toast: Option<String>,
}

/// Chat background picker view-model and UI (`boxes/background_box.cpp` + `background_preview_box.cpp`).
pub struct BackgroundPickerState {
    /// Active category filter tab.
    pub category: BackgroundCategory,
    /// Full gallery catalog (built-in gradients, patterns, solid colors, and custom images).
    items: Vec<BackgroundGalleryItem>,
    /// Currently selected item ID in the gallery.
    selected_id: String,
    /// Draft file path for "Choose from file".
    pub custom_file_path: String,
    /// Embedded live preview panel state.
    pub preview: BackgroundPreviewState,
}

impl BackgroundPickerState {
    /// Creates a new background picker populated with Telegram-style GPL-safe presets.
    #[must_use]
    pub fn new(dark_mode: bool, target_peer: Option<(i64, String)>) -> Self {
        let items = builtin_gallery_items();
        let first = items
            .first()
            .map(|i| i.config.clone())
            .unwrap_or_else(BackgroundConfig::classic_meadow);
        let selected_id = items.first().map(|i| i.id.clone()).unwrap_or_default();
        Self {
            category: BackgroundCategory::All,
            items,
            selected_id,
            custom_file_path: String::new(),
            preview: BackgroundPreviewState::new(first, dark_mode, target_peer),
        }
    }

    /// Returns all gallery items.
    #[must_use]
    pub fn items(&self) -> &[BackgroundGalleryItem] {
        &self.items
    }

    /// Returns the currently selected gallery item ID.
    #[must_use]
    pub fn selected_id(&self) -> &str {
        &self.selected_id
    }

    /// Returns items matching the active [`Self::category`] filter.
    #[must_use]
    pub fn visible_items(&self) -> Vec<&BackgroundGalleryItem> {
        self.items
            .iter()
            .filter(|item| {
                self.category == BackgroundCategory::All || item.category == self.category
            })
            .collect()
    }

    /// Selects a gallery item by ID and loads it into [`Self::preview`].
    pub fn select_by_id(&mut self, id: &str) -> bool {
        if let Some(item) = self.items.iter().find(|i| i.id == id) {
            self.selected_id = item.id.clone();
            self.preview
                .set_config_with_cloud(item.config.clone(), item.cloud_record.clone());
            true
        } else {
            false
        }
    }

    /// Merges cloud wallpapers returned by `account.getWallPapers` / `account.getMultiWallPapers`
    /// into the gallery, preserving pattern, blur, motion, rotation, and colors.
    pub fn merge_cloud_wallpapers(&mut self, records: &[CloudWallPaperRecord]) {
        for rec in records {
            let id = if rec.wallpaper.id != 0 {
                format!("cloud_wp_{}", rec.wallpaper.id)
            } else if !rec.wallpaper.slug.is_empty() {
                format!("cloud_wp_{}", rec.wallpaper.slug)
            } else {
                continue;
            };
            let pattern = if rec.wallpaper.is_pattern() {
                PatternKind::from_slug(&rec.wallpaper.slug)
            } else {
                PatternKind::None
            };
            let category = if rec.wallpaper.is_pattern() {
                BackgroundCategory::Patterns
            } else if rec.wallpaper.background_colors.len() >= 2 {
                BackgroundCategory::Gradients
            } else if rec.wallpaper.background_colors.len() == 1 {
                BackgroundCategory::Solid
            } else {
                BackgroundCategory::Custom
            };
            let name = if !rec.wallpaper.slug.is_empty() {
                rec.wallpaper.slug.clone()
            } else {
                format!("Cloud #{}", rec.wallpaper.id)
            };
            let cfg = BackgroundConfig {
                wallpaper: rec.wallpaper.clone(),
                pattern,
                pattern_bytes: None,
                custom_image: None,
                tiled: rec.wallpaper.is_pattern(),
                motion: rec.motion,
                dark_dimming: 0,
            };
            let item = BackgroundGalleryItem {
                id: id.clone(),
                name,
                category,
                config: cfg,
                removable: !rec.wallpaper.is_default_flag(),
                cloud_record: Some(rec.clone()),
            };
            if let Some(existing) = self.items.iter_mut().find(|i| i.id == id) {
                let prev_pattern_bytes = existing.config.pattern_bytes.clone();
                *existing = item;
                if existing.config.pattern_bytes.is_none() {
                    existing.config.pattern_bytes = prev_pattern_bytes;
                }
            } else {
                self.items.insert(0, item);
            }
        }
    }

    /// Downloads `.tgv` / SVG pattern document bytes for cloud pattern wallpapers via
    /// [`ThemesBackend::download_wallpaper_bytes`] and attaches them so [`ChatBackground`]
    /// rasterizes the real server vector pattern instead of falling back to procedural masks.
    pub fn hydrate_cloud_pattern_bytes(&mut self, backend: &dyn ThemesBackend) -> usize {
        let mut hydrated = 0usize;
        for item in &mut self.items {
            if !item.config.wallpaper.is_pattern() || item.config.has_svg_pattern() {
                continue;
            }
            let Some(ref rec) = item.cloud_record else {
                continue;
            };
            let Some(ref doc) = rec.document else {
                continue;
            };
            let is_pattern_mime = doc
                .mime_type
                .eq_ignore_ascii_case("application/x-tgwallpattern")
                || doc.mime_type.eq_ignore_ascii_case("image/svg+xml")
                || doc.mime_type.eq_ignore_ascii_case("application/gzip");
            if !is_pattern_mime && !rec.wallpaper.is_pattern() {
                continue;
            }
            if let Ok(bytes) = backend.download_wallpaper_bytes(doc)
                && ChatBackground::is_svg_or_tgv_pattern_bytes(&bytes)
            {
                item.config = item.config.clone().with_pattern_bytes(bytes);
                if self.selected_id == item.id {
                    self.preview
                        .set_config_with_cloud(item.config.clone(), item.cloud_record.clone());
                }
                hydrated += 1;
            }
        }
        hydrated
    }

    /// Removes a user-added custom or cloud wallpaper by ID (`BackgroundBox::removePaper`),
    /// returning its [`CloudWallPaperRecord`] if it was a cloud wallpaper.
    pub fn remove_custom_with_cloud(&mut self, id: &str) -> (bool, Option<CloudWallPaperRecord>) {
        if let Some(pos) = self.items.iter().position(|i| i.id == id && i.removable) {
            let removed = self.items.remove(pos);
            if self.selected_id == id
                && let Some(first) = self.items.first()
            {
                self.selected_id = first.id.clone();
                self.preview
                    .set_config_with_cloud(first.config.clone(), first.cloud_record.clone());
            }
            (true, removed.cloud_record)
        } else {
            (false, None)
        }
    }

    /// Removes a user-added custom wallpaper by ID (`BackgroundBox::removePaper`).
    pub fn remove_custom(&mut self, id: &str) -> bool {
        self.remove_custom_with_cloud(id).0
    }

    /// Loads an image file (`.png`, `.jpg`, `.webp`, `.gif`) or vector pattern wallpaper
    /// (`.svg`, `.tgv`) from disk and adds it to the gallery + opens it in preview.
    pub fn add_from_file(&mut self, path: &Path) -> Result<String, String> {
        let raw_bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        let (cfg, category) = if ext == "tgv"
            || ext == "svg"
            || ChatBackground::is_svg_or_tgv_pattern_bytes(&raw_bytes)
        {
            let mut wp = WallPaper::construct_default();
            wp.id = tg_theme::CUSTOM_BACKGROUND_ID;
            wp.slug = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("custom_pattern")
                .to_owned();
            let cfg = ChatBackground::load_pattern_from_bytes(&raw_bytes, wp)
                .map_err(|e| e.to_string())?;
            (cfg, BackgroundCategory::Patterns)
        } else {
            let cfg =
                ChatBackground::load_config_from_bytes(&raw_bytes).map_err(|e| e.to_string())?;
            (cfg, BackgroundCategory::Custom)
        };
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("Custom Background")
            .to_owned();
        let id = format!("custom_{}", self.items.len() + 1);
        self.items.insert(
            0,
            BackgroundGalleryItem {
                id: id.clone(),
                name: name.clone(),
                category,
                config: cfg.clone(),
                removable: true,
                cloud_record: None,
            },
        );
        self.selected_id = id;
        self.preview.set_config_with_cloud(cfg, None);
        Ok(name)
    }

    /// Renders the two-column Chat Background Picker (left: gallery grid + file loader;
    /// right: interactive preview with sample messages & apply buttons).
    pub fn show(
        &mut self,
        ui: &mut Ui,
        chat_bg: &mut ChatBackground,
        colors: &dyn Colors,
    ) -> BackgroundPickerOutput {
        let mut out = BackgroundPickerOutput::default();
        let bold_fg = colors.color("windowBoldFg");
        let sub_fg = colors.color("windowSubTextFg");
        let active_bg = colors.color("windowBgActive");
        let divider = colors.color("shadowFg");
        let bg_over = colors.color("windowBgOver");

        let avail = ui.available_width();
        let side_by_side = avail >= 740.0;

        if side_by_side {
            ui.columns(2, |cols| {
                self.show_gallery_column(
                    &mut cols[0],
                    chat_bg,
                    bold_fg,
                    sub_fg,
                    active_bg,
                    divider,
                    bg_over,
                    &mut out,
                );
                let prev_out = self.preview.show(&mut cols[1], chat_bg, colors);
                if prev_out.applied.is_some() {
                    out.applied = prev_out.applied;
                }
                if prev_out.save_cloud_requested {
                    out.save_cloud_requested = true;
                }
                if prev_out.reset_peer_requested.is_some() {
                    out.reset_peer_requested = prev_out.reset_peer_requested;
                }
                if prev_out.toast.is_some() {
                    out.toast = prev_out.toast;
                }
            });
        } else {
            self.show_gallery_column(
                ui, chat_bg, bold_fg, sub_fg, active_bg, divider, bg_over, &mut out,
            );
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);
            let prev_out = self.preview.show(ui, chat_bg, colors);
            if prev_out.applied.is_some() {
                out.applied = prev_out.applied;
            }
            if prev_out.save_cloud_requested {
                out.save_cloud_requested = true;
            }
            if prev_out.reset_peer_requested.is_some() {
                out.reset_peer_requested = prev_out.reset_peer_requested;
            }
            if prev_out.toast.is_some() {
                out.toast = prev_out.toast;
            }
        }

        out
    }

    #[allow(clippy::too_many_arguments)]
    fn show_gallery_column(
        &mut self,
        ui: &mut Ui,
        chat_bg: &mut ChatBackground,
        bold_fg: Color32,
        sub_fg: Color32,
        active_bg: Color32,
        divider: Color32,
        bg_over: Color32,
        out: &mut BackgroundPickerOutput,
    ) {
        ui.label(
            egui::RichText::new("Chat Background Gallery")
                .size(15.0)
                .strong()
                .color(bold_fg),
        );
        ui.add_space(6.0);

        // Choose from file bar (`lng_settings_bg_from_file`)
        Frame::NONE
            .fill(bg_over)
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::same(8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.custom_file_path)
                            .desired_width((ui.available_width() - 122.0).max(100.0))
                            .hint_text("Path (.png, .jpg, .webp, .svg, .tgv)..."),
                    );
                    if ui.button(tr::lng_settings_bg_from_file()).clicked() {
                        let path = Path::new(self.custom_file_path.trim()).to_path_buf();
                        match self.add_from_file(&path) {
                            Ok(name) => {
                                out.toast = Some(format!("Loaded background '{name}'"));
                            }
                            Err(err) => {
                                out.toast = Some(format!(
                                    "Could not load background ({err}). Check the file path and try again."
                                ));
                            }
                        }
                    }
                });
            });

        ui.add_space(8.0);

        // Category filter pills
        ui.horizontal_wrapped(|ui| {
            for &cat in BackgroundCategory::ALL {
                if ui
                    .selectable_label(self.category == cat, cat.label())
                    .clicked()
                {
                    self.category = cat;
                }
            }
        });

        ui.add_space(8.0);

        // 3-column thumbnail grid (`kBackgroundsInRow = 3` in `background_box.cpp`)
        let visible_ids: Vec<(String, String, BackgroundConfig, bool)> = self
            .items
            .iter()
            .filter(|item| {
                self.category == BackgroundCategory::All || item.category == self.category
            })
            .map(|i| (i.id.clone(), i.name.clone(), i.config.clone(), i.removable))
            .collect();

        let mut clicked_id: Option<String> = None;
        let mut remove_id: Option<String> = None;

        ScrollArea::vertical()
            .id_salt("tg_background_gallery_scroll")
            .max_height(380.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let cols = 3usize;
                let gap = 8.0;
                let tile_w = ((ui.available_width() - gap * (cols as f32 - 1.0)) / cols as f32)
                    .clamp(76.0, 140.0);
                let tile_h = (tile_w * 1.18).round();

                for row_chunk in visible_ids.chunks(cols) {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = gap;
                        for (id, name, cfg, removable) in row_chunk {
                            let is_selected = self.selected_id == *id;
                            let (rect, resp) =
                                ui.allocate_exact_size(Vec2::new(tile_w, tile_h), Sense::click());
                            let aria_name = format!("Background: {name}");
                            resp.widget_info(|| {
                                WidgetInfo::selected(
                                    Role::Button,
                                    true,
                                    is_selected,
                                    aria_name.clone(),
                                )
                            });
                            if resp.hovered() {
                                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                            }
                            let thumb_rect =
                                Rect::from_min_size(rect.min, Vec2::new(tile_w, tile_h - 20.0));
                            chat_bg.paint_config(
                                ui,
                                thumb_rect,
                                cfg,
                                self.preview.preview_dark_mode,
                            );
                            let stroke = if is_selected || resp.has_focus() {
                                Stroke::new(2.5, active_bg)
                            } else {
                                Stroke::new(1.0, divider)
                            };
                            ui.painter().rect_stroke(
                                thumb_rect,
                                CornerRadius::same(8),
                                stroke,
                                StrokeKind::Inside,
                            );

                            // Active checkmark badge (`_check` in `background_box.cpp`)
                            if is_selected {
                                let c = Pos2::new(
                                    thumb_rect.right() - 14.0,
                                    thumb_rect.bottom() - 14.0,
                                );
                                ui.painter().circle_filled(c, 10.0, active_bg);
                                ui.painter().line_segment(
                                    [
                                        Pos2::new(c.x - 4.0, c.y + 0.5),
                                        Pos2::new(c.x - 1.0, c.y + 3.5),
                                    ],
                                    Stroke::new(2.0, Color32::WHITE),
                                );
                                ui.painter().line_segment(
                                    [
                                        Pos2::new(c.x - 1.0, c.y + 3.5),
                                        Pos2::new(c.x + 4.5, c.y - 3.0),
                                    ],
                                    Stroke::new(2.0, Color32::WHITE),
                                );
                            }

                            // Hover delete badge for removable custom wallpapers
                            if *removable && resp.hovered() {
                                let del_rect = Rect::from_min_size(
                                    Pos2::new(thumb_rect.right() - 22.0, thumb_rect.top() + 4.0),
                                    Vec2::splat(18.0),
                                );
                                ui.painter().rect_filled(
                                    del_rect,
                                    CornerRadius::same(9),
                                    Color32::from_rgba_unmultiplied(0, 0, 0, 160),
                                );
                                ui.painter().text(
                                    del_rect.center(),
                                    Align2::CENTER_CENTER,
                                    "×",
                                    FontId::proportional(12.0),
                                    Color32::WHITE,
                                );
                                if resp.clicked()
                                    && let Some(pos) = resp.interact_pointer_pos()
                                    && del_rect.contains(pos)
                                {
                                    remove_id = Some(id.clone());
                                    continue;
                                }
                            }

                            ui.painter().text(
                                Pos2::new(rect.center().x, thumb_rect.bottom() + 4.0),
                                Align2::CENTER_TOP,
                                name,
                                FontId::proportional(11.0),
                                if is_selected { bold_fg } else { sub_fg },
                            );

                            if resp.clicked()
                                || (resp.has_focus()
                                    && ui.input(|i| {
                                        i.key_pressed(egui::Key::Enter)
                                            || i.key_pressed(egui::Key::Space)
                                    }))
                            {
                                clicked_id = Some(id.clone());
                            }
                        }
                    });
                    ui.add_space(gap);
                }
            });

        if let Some(rem) = remove_id {
            let (removed, cloud_rec) = self.remove_custom_with_cloud(&rem);
            if removed {
                out.removed_cloud = cloud_rec;
                out.toast = Some("Removed wallpaper".to_owned());
            }
        } else if let Some(sel) = clicked_id {
            self.select_by_id(&sel);
        }
    }
}

/// Paints realistic Telegram sample messages over the preview background
/// (`BackgroundPreviewBox::paintTexts` in `background_preview_box.cpp`).
fn paint_sample_messages(ui: &Ui, rect: Rect, dark_mode: bool, for_single_chat: bool) {
    let pal = prepare_base_palette(dark_mode, None).unwrap_or_default();
    let msg_in_bg = theme_color_to_egui(pal.msg_in_bg);
    let msg_out_bg = theme_color_to_egui(pal.msg_out_bg);
    let text_in = theme_color_to_egui(pal.history_text_in_fg);
    let text_out = theme_color_to_egui(pal.history_text_out_fg);
    let date_in = theme_color_to_egui(pal.msg_in_date_fg);
    let date_out = theme_color_to_egui(pal.msg_out_date_fg);
    let service_bg = theme_color_to_egui(pal.msg_service_bg);
    let service_fg = theme_color_to_egui(pal.msg_service_fg);

    // Date service pill
    let pill = Rect::from_center_size(
        Pos2::new(rect.center().x, rect.top() + 24.0),
        Vec2::new(82.0, 22.0),
    );
    ui.painter()
        .rect_filled(pill, CornerRadius::same(11), service_bg);
    ui.painter().text(
        pill.center(),
        Align2::CENTER_CENTER,
        "Today",
        FontId::proportional(11.5),
        service_fg,
    );

    let (line1, line2) = if for_single_chat {
        (tr::lng_background_apply1(), tr::lng_background_apply2())
    } else {
        (
            "How does this chat background look?".to_owned(),
            "Clean and legible in Day and Night modes.".to_owned(),
        )
    };

    // Incoming message bubble (left) — constrained to canvas width so narrow viewports never clip
    let max_bubble_w = (rect.width() - 32.0).max(140.0);
    let in_w = (rect.width() * 0.72).min(280.0).min(max_bubble_w);
    let in_rect = Rect::from_min_size(
        Pos2::new(rect.left() + 16.0, pill.bottom() + 14.0),
        Vec2::new(in_w, 56.0),
    );
    ui.painter().rect_filled(
        in_rect.translate(Vec2::new(0.0, 1.5)),
        CornerRadius::same(12),
        Color32::from_rgba_unmultiplied(0, 0, 0, 32),
    );
    ui.painter()
        .rect_filled(in_rect, CornerRadius::same(12), msg_in_bg);
    let in_galley = ui.fonts_mut(|f| {
        f.layout(
            line1,
            FontId::proportional(12.5),
            text_in,
            (in_w - 24.0).max(80.0),
        )
    });
    ui.painter().galley(
        Pos2::new(in_rect.left() + 12.0, in_rect.top() + 8.0),
        in_galley,
        text_in,
    );
    ui.painter().text(
        Pos2::new(in_rect.right() - 10.0, in_rect.bottom() - 8.0),
        Align2::RIGHT_BOTTOM,
        "10:42",
        FontId::proportional(10.5),
        date_in,
    );

    // Outgoing message bubble (right)
    let out_w = (rect.width() * 0.74).min(290.0).min(max_bubble_w);
    let out_rect = Rect::from_min_size(
        Pos2::new(rect.right() - out_w - 16.0, in_rect.bottom() + 14.0),
        Vec2::new(out_w, 56.0),
    );
    ui.painter().rect_filled(
        out_rect.translate(Vec2::new(0.0, 1.5)),
        CornerRadius::same(12),
        Color32::from_rgba_unmultiplied(0, 0, 0, 32),
    );
    ui.painter()
        .rect_filled(out_rect, CornerRadius::same(12), msg_out_bg);
    let out_galley = ui.fonts_mut(|f| {
        f.layout(
            line2,
            FontId::proportional(12.5),
            text_out,
            (out_w - 24.0).max(80.0),
        )
    });
    ui.painter().galley(
        Pos2::new(out_rect.left() + 12.0, out_rect.top() + 8.0),
        out_galley,
        text_out,
    );
    ui.painter().text(
        Pos2::new(out_rect.right() - 28.0, out_rect.bottom() - 8.0),
        Align2::RIGHT_BOTTOM,
        "10:43",
        FontId::proportional(10.5),
        date_out,
    );
    // Double read-checkmarks (vector strokes so no missing glyph boxes)
    let tick_y = out_rect.bottom() - 12.0;
    for dx in [0.0f32, 4.5f32] {
        let bx = out_rect.right() - 22.0 + dx;
        ui.painter().line_segment(
            [Pos2::new(bx, tick_y), Pos2::new(bx + 2.5, tick_y + 2.8)],
            Stroke::new(1.4, date_out),
        );
        ui.painter().line_segment(
            [
                Pos2::new(bx + 2.5, tick_y + 2.8),
                Pos2::new(bx + 7.0, tick_y - 2.8),
            ],
            Stroke::new(1.4, date_out),
        );
    }
}

/// Built-in GPL-safe wallpaper gallery covering 4-color gradients, 2-color linear gradients,
/// pattern overlays (light & dark), and solid colors.
#[must_use]
pub fn builtin_gallery_items() -> Vec<BackgroundGalleryItem> {
    let mut items = vec![
        BackgroundGalleryItem {
            id: "classic_meadow".to_owned(),
            name: "Classic Meadow".to_owned(),
            category: BackgroundCategory::Patterns,
            config: BackgroundConfig::classic_meadow(),
            removable: false,
            cloud_record: None,
        },
        BackgroundGalleryItem {
            id: "night_aurora".to_owned(),
            name: "Night Aurora".to_owned(),
            category: BackgroundCategory::Patterns,
            config: BackgroundConfig::night_aurora(),
            removable: false,
            cloud_record: None,
        },
        BackgroundGalleryItem {
            id: "sunset_bloom".to_owned(),
            name: "Sunset Bloom".to_owned(),
            category: BackgroundCategory::Gradients,
            config: BackgroundConfig::gradient(
                vec![
                    Color::rgb(0xFE, 0xC4, 0x96),
                    Color::rgb(0xDD, 0x6C, 0xB9),
                    Color::rgb(0x96, 0x2F, 0xBF),
                    Color::rgb(0x4F, 0x5B, 0xD5),
                ],
                45,
                PatternKind::None,
                0,
                true,
            ),
            removable: false,
            cloud_record: None,
        },
        BackgroundGalleryItem {
            id: "arctic_waves".to_owned(),
            name: "Arctic Waves".to_owned(),
            category: BackgroundCategory::Patterns,
            config: BackgroundConfig::gradient(
                vec![
                    Color::rgb(0xB8, 0xE1, 0xFC),
                    Color::rgb(0x74, 0xB9, 0xE7),
                    Color::rgb(0xD3, 0xEC, 0xFA),
                    Color::rgb(0x88, 0xC8, 0xF0),
                ],
                90,
                PatternKind::Waves,
                40,
                true,
            ),
            removable: false,
            cloud_record: None,
        },
        BackgroundGalleryItem {
            id: "botanical_sage".to_owned(),
            name: "Botanical Sage".to_owned(),
            category: BackgroundCategory::Patterns,
            config: BackgroundConfig::gradient(
                vec![
                    Color::rgb(0xD4, 0xE7, 0xC5),
                    Color::rgb(0x99, 0xBC, 0x85),
                    Color::rgb(0xE1, 0xF0, 0xDA),
                    Color::rgb(0xBF, 0xD8, 0xAF),
                ],
                135,
                PatternKind::Botanical,
                45,
                false,
            ),
            removable: false,
            cloud_record: None,
        },
        BackgroundGalleryItem {
            id: "cyber_hex".to_owned(),
            name: "Midnight Hex".to_owned(),
            category: BackgroundCategory::Patterns,
            config: {
                let mut cfg = BackgroundConfig::gradient(
                    vec![
                        Color::rgb(0x14, 0x28, 0x3D),
                        Color::rgb(0x1E, 0x4D, 0x6B),
                        Color::rgb(0x12, 0x22, 0x33),
                    ],
                    0,
                    PatternKind::Hexagons,
                    -48,
                    true,
                );
                cfg.wallpaper.flags = cfg.wallpaper.flags.union(WallPaperFlags::DARK);
                cfg
            },
            removable: false,
            cloud_record: None,
        },
        BackgroundGalleryItem {
            id: "linear_sky".to_owned(),
            name: "Azure Linear".to_owned(),
            category: BackgroundCategory::Gradients,
            config: BackgroundConfig::gradient(
                vec![Color::rgb(0xD8, 0xED, 0xFA), Color::rgb(0x7B, 0xBF, 0xE8)],
                45,
                PatternKind::None,
                0,
                false,
            ),
            removable: false,
            cloud_record: None,
        },
        BackgroundGalleryItem {
            id: "linear_ember".to_owned(),
            name: "Warm Dusk".to_owned(),
            category: BackgroundCategory::Gradients,
            config: BackgroundConfig::gradient(
                vec![Color::rgb(0x2B, 0x1E, 0x3E), Color::rgb(0x6E, 0x35, 0x52)],
                135,
                PatternKind::None,
                0,
                false,
            ),
            removable: false,
            cloud_record: None,
        },
        BackgroundGalleryItem {
            id: "solid_day_mist".to_owned(),
            name: "Day Mist".to_owned(),
            category: BackgroundCategory::Solid,
            config: BackgroundConfig::solid(Color::rgb(0xD5, 0xE2, 0xEC)),
            removable: false,
            cloud_record: None,
        },
        BackgroundGalleryItem {
            id: "solid_cream".to_owned(),
            name: "Warm Parchment".to_owned(),
            category: BackgroundCategory::Solid,
            config: BackgroundConfig::solid(Color::rgb(0xF3, 0xEC, 0xDF)),
            removable: false,
            cloud_record: None,
        },
        BackgroundGalleryItem {
            id: "solid_midnight".to_owned(),
            name: "Midnight Slate".to_owned(),
            category: BackgroundCategory::Solid,
            config: BackgroundConfig::solid(Color::rgb(0x0E, 0x16, 0x21)),
            removable: false,
            cloud_record: None,
        },
        BackgroundGalleryItem {
            id: "solid_obsidian".to_owned(),
            name: "Obsidian".to_owned(),
            category: BackgroundCategory::Solid,
            config: BackgroundConfig::solid(Color::rgb(0x18, 0x18, 0x1E)),
            removable: false,
            cloud_record: None,
        },
    ];

    // Ensure every built-in item has a valid shareable color slug
    for item in &mut items {
        if item.config.wallpaper.slug.is_empty()
            && !item.config.wallpaper.background_colors.is_empty()
        {
            item.config.wallpaper = WallPaper::from_colors_slug(&tg_theme::string_from_colors(
                &item.config.wallpaper.background_colors,
            ))
            .unwrap_or_else(|| item.config.wallpaper.clone())
            .with_params_from(&item.config.wallpaper);
        }
    }
    items
}
