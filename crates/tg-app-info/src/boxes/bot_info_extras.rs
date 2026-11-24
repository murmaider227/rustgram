//! Bot Info Extras (`BotIntroPreview`, `TranslateBotInfoBox`) and `MusicAttachBox`.
//!
//! Port origin:
//! - `Telegram/SourceFiles/boxes/peers/bot_intro_preview.h` / `.cpp`
//! - `Telegram/SourceFiles/boxes/peers/translate_bot_info_box.h` / `.cpp`
//! - `Telegram/SourceFiles/boxes/music_attach_box.h` / `.cpp`
//! - `Telegram/SourceFiles/boxes/peers/edit_peer_members.style`

use std::collections::BTreeSet;

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Sense, Shape, Stroke,
    StrokeKind, Ui, Vec2,
};
use tg_lang::tr;
use tg_ui::anim::{SimpleAnimation, Transition, interpolate_f32, with_alpha};
use tg_ui::fonts::{family_regular, family_semibold, font_id_from_style};
use tg_ui::icons::paint_style_icon;
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{Colors, FlatLabelStyle, InputFieldStyle};
use tg_ui::tg_style::st;
use tg_ui::theme::ColorsResolver;
use tg_ui::widgets::input_field::InputField;
use tg_ui::widgets::labels::{FlatLabel, divider_bar};

use crate::boxes::managed_bot::{BOT_INFO_LANGUAGES, BotInfoDraft};
use crate::info::backend::{InfoBackend, SavedMusicTrack};
use tg_app_core::stand_in::types::PeerId;

/// Vertical skip inside the Bot Intro preview wallpaper strip (`st::botIntroPreviewSkip = 12px`).
pub const BOT_INTRO_PREVIEW_SKIP: f32 = 12.0;
/// Bubble corner radius (`st::bubbleRadiusLarge = 15px`).
pub const BOT_INTRO_BUBBLE_RADIUS: f32 = 15.0;
/// Bubble resize animation duration (`st::slideWrapDuration = 160ms`).
pub const BOT_INTRO_RESIZE_DURATION_MS: u32 = 160;
/// Diameter of the translation progress ring (`st::botInfoTranslateRadialSize = 64px`).
pub const TRANSLATE_RADIAL_SIZE: f32 = 64.0;
/// Stroke thickness of the translation progress ring (`st::botInfoTranslateRadialStroke = 3px`).
pub const TRANSLATE_RADIAL_STROKE: f32 = 3.0;
/// Maximum number of tracks that can be attached at once (`MaxSelectedItems = 10`).
pub const MAX_SELECTED_MUSIC_TRACKS: usize = 10;

// ============================================================================
// 1. BotIntroPreview (`boxes/peers/bot_intro_preview.cpp`)
// ============================================================================

/// Animated state for the live Bot Intro chat bubble preview (`Preview` in `bot_intro_preview.cpp`).
#[derive(Debug, Clone)]
pub struct BotIntroPreviewState {
    /// Bot display name.
    pub bot_name: String,
    /// Optional intro media caption/badge (when the bot has a photo/animation intro).
    pub media_label: Option<String>,
    /// Last rendered description text (used to trigger smooth height interpolation on change).
    last_text: String,
    /// Previous measured bubble height before the last edit.
    from_height: f32,
    /// Target measured bubble height.
    target_height: f32,
    /// Resize interpolation animation (`_resize` with `easeOutCirc`).
    resize_anim: SimpleAnimation,
}

impl Default for BotIntroPreviewState {
    fn default() -> Self {
        Self::new("Bot", None)
    }
}

impl BotIntroPreviewState {
    /// Creates a new [`BotIntroPreviewState`] for `bot_name`.
    #[must_use]
    pub fn new(bot_name: impl Into<String>, media_label: Option<String>) -> Self {
        Self {
            bot_name: bot_name.into(),
            media_label,
            last_text: String::new(),
            from_height: 0.0,
            target_height: 0.0,
            resize_anim: SimpleAnimation::new(),
        }
    }

    /// Paints the framed Telegram chat wallpaper strip containing the centered incoming bot intro
    /// bubble (`CreateBotIntroPreview` / `Preview::paintEvent`).
    pub fn show(&mut self, ui: &mut Ui, description: &str, colors: &dyn Colors) {
        let trimmed = description.trim();
        if trimmed.is_empty() && self.media_label.is_none() {
            self.last_text.clear();
            self.target_height = 0.0;
            return;
        }

        let avail_w = ui.available_width().max(180.0);
        let bubble_w = (avail_w - 32.0).clamp(160.0, 320.0);
        let inner_w = (bubble_w - 28.0).max(120.0);

        let title_font = FontId::new(13.5, family_semibold());
        let body_font = FontId::new(13.0, family_regular());
        let title_galley = ui.painter().layout_no_wrap(
            "What can this bot do?".to_owned(),
            title_font,
            colors.color("windowBoldFg"),
        );
        let body_galley = ui.painter().layout(
            trimmed.to_owned(),
            body_font,
            colors.color("windowFg"),
            inner_w,
        );

        let media_h = if self.media_label.is_some() {
            92.0
        } else {
            0.0
        };
        let measured_bubble_h = 12.0
            + media_h
            + if media_h > 0.0 { 8.0 } else { 0.0 }
            + title_galley.size().y
            + 4.0
            + body_galley.size().y
            + 12.0;

        let now = ui.ctx().input(|i| i.time);
        if self.last_text != trimmed {
            if self.target_height > 0.0 && (self.target_height - measured_bubble_h).abs() > 1.0 {
                self.from_height = self.current_height(now);
                self.resize_anim.start(
                    now,
                    0.0,
                    1.0,
                    BOT_INTRO_RESIZE_DURATION_MS,
                    Transition::EaseOutCirc,
                );
            } else {
                self.from_height = measured_bubble_h;
            }
            self.target_height = measured_bubble_h;
            self.last_text = trimmed.to_owned();
        } else if self.target_height <= 0.0 {
            self.from_height = measured_bubble_h;
            self.target_height = measured_bubble_h;
        }

        let animated_bubble_h = self.current_height(now);
        if self.resize_anim.animating() {
            ui.ctx().request_repaint();
        }

        let total_h = animated_bubble_h + 2.0 * BOT_INTRO_PREVIEW_SKIP;
        let (strip_rect, _) = ui.allocate_exact_size(Vec2::new(avail_w, total_h), Sense::hover());

        // Chat wallpaper backdrop (matching tdesktop's classic soft tinted chat canvas).
        let is_dark = colors.color("windowBg").r() < 128;
        let wall_bg = if is_dark {
            Color32::from_rgb(0x16, 0x22, 0x30)
        } else {
            Color32::from_rgb(0xD8, 0xE6, 0xD2)
        };
        let wall_border = colors.color("shadowFg");
        ui.painter()
            .rect_filled(strip_rect, CornerRadius::same(10), wall_bg);
        ui.painter().rect_stroke(
            strip_rect,
            CornerRadius::same(10),
            Stroke::new(1.0, with_alpha(wall_border, 0.28)),
            StrokeKind::Inside,
        );

        // Subtle wallpaper pattern dots for authentic chat depth.
        let dot_color = if is_dark {
            Color32::from_rgba_unmultiplied(255, 255, 255, 10)
        } else {
            Color32::from_rgba_unmultiplied(36, 86, 48, 14)
        };
        let mut py = strip_rect.top() + 14.0;
        let mut row_i = 0usize;
        while py < strip_rect.bottom() - 8.0 {
            let mut px = strip_rect.left() + 18.0 + if row_i % 2 == 1 { 16.0 } else { 0.0 };
            while px < strip_rect.right() - 12.0 {
                ui.painter()
                    .circle_filled(Pos2::new(px, py), 1.6, dot_color);
                px += 32.0;
            }
            py += 24.0;
            row_i += 1;
        }

        // Centered incoming message bubble (`msgInBg` + `bubbleRadiusLarge`).
        let bubble_rect =
            Rect::from_center_size(strip_rect.center(), Vec2::new(bubble_w, animated_bubble_h));
        let bubble_radius = CornerRadius::same(BOT_INTRO_BUBBLE_RADIUS as u8);
        let shadow_rect = bubble_rect.translate(Vec2::new(0.0, 1.5));
        ui.painter().rect_filled(
            shadow_rect,
            bubble_radius,
            Color32::from_rgba_unmultiplied(0, 0, 0, if is_dark { 45 } else { 22 }),
        );
        let bubble_bg = colors.color("windowBg");
        ui.painter()
            .rect_filled(bubble_rect, bubble_radius, bubble_bg);

        let clipped = ui.painter().with_clip_rect(bubble_rect);
        let mut cursor_y = bubble_rect.top() + 12.0;
        let text_left = bubble_rect.left() + 14.0;

        if let Some(ref media) = self.media_label {
            let media_rect = Rect::from_min_size(
                Pos2::new(bubble_rect.left() + 8.0, cursor_y),
                Vec2::new(bubble_rect.width() - 16.0, media_h),
            );
            clipped.rect_filled(
                media_rect,
                CornerRadius::same(10),
                with_alpha(colors.color("windowBgActive"), 0.14),
            );
            clipped.text(
                media_rect.center(),
                Align2::CENTER_CENTER,
                media,
                FontId::new(12.0, family_semibold()),
                colors.color("windowActiveTextFg"),
            );
            cursor_y += media_h + 8.0;
        }

        clipped.galley(
            Pos2::new(text_left, cursor_y),
            title_galley,
            colors.color("windowBoldFg"),
        );
        cursor_y += 20.0;
        clipped.galley(
            Pos2::new(text_left, cursor_y),
            body_galley,
            colors.color("windowFg"),
        );
    }

    fn current_height(&mut self, now_secs: f64) -> f32 {
        let p = self.resize_anim.sample(now_secs, 1.0) as f32;
        interpolate_f32(self.from_height, self.target_height, p)
    }
}

/// Standalone modal wrapper around [`BotIntroPreviewState`] (`BoxKind::BotIntroPreview`).
#[derive(Debug, Clone)]
pub struct BotIntroPreviewBoxState {
    /// Target bot peer ID.
    pub bot_id: PeerId,
    /// Target bot display name.
    pub bot_name: String,
    /// Editable preview description text.
    pub description: String,
    /// Inner animated preview state.
    pub preview: BotIntroPreviewState,
}

impl BotIntroPreviewBoxState {
    /// Creates a new [`BotIntroPreviewBoxState`].
    #[must_use]
    pub fn new(
        bot_id: PeerId,
        bot_name: impl Into<String>,
        description: impl Into<String>,
        media_label: Option<String>,
    ) -> Self {
        let name = bot_name.into();
        Self {
            bot_id,
            bot_name: name.clone(),
            description: description.into(),
            preview: BotIntroPreviewState::new(name, media_label),
        }
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        BoxModal::new(id, format!("{} — Intro Preview", self.bot_name))
            .width(st::box_width.0 as f32 + 24.0)
            .add_button(BoxButton::default_btn("close", tr::lng_close()))
    }

    /// Renders the standalone Bot Intro Preview modal body.
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) {
        self.preview.show(ui, &self.description, colors);
        ui.add_space(10.0);
        let input_st = InputFieldStyle::default_input(colors);
        InputField::multi_line(
            "bot_intro_preview_desc",
            &mut self.description,
            "Description (shown in empty chat)",
            input_st,
        )
        .show(ui);
    }
}

// ============================================================================
// 2. TranslateBotInfoBox (`boxes/peers/translate_bot_info_box.cpp`)
// ============================================================================

/// Execution status of a multi-language bot info translation batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranslateJobStatus {
    /// Waiting for the user to choose target languages and click Translate.
    Selecting,
    /// Actively translating language codes (`done` of `total`).
    Translating {
        /// Number of languages translated so far.
        done: usize,
        /// Total languages queued.
        total: usize,
    },
    /// Paused due to server flood-wait (`seconds_left` remaining).
    Paused {
        /// Remaining cooldown seconds.
        seconds_left: u32,
        /// Completed count before pause.
        done: usize,
        /// Total languages queued.
        total: usize,
    },
    /// Stopped early due to server limit or user cancellation.
    Stopped(String),
    /// Completed batch translation (`translated_count` languages updated).
    Finished {
        /// Number of languages successfully translated.
        translated_count: usize,
        /// Whether translations have been saved via `bots.setBotInfo`.
        saved: bool,
    },
}

/// Multi-language Bot Info translation modal (`boxes/peers/translate_bot_info_box.cpp`).
#[derive(Debug, Clone)]
pub struct TranslateBotInfoBoxState {
    /// Bot peer ID.
    pub bot_id: PeerId,
    /// Bot display name or username.
    pub bot_name: String,
    /// Selected target ISO language codes (non-empty codes, e.g. `"es"`, `"de"`).
    pub selected_codes: Vec<String>,
    /// Queue of remaining language codes to translate.
    pending_codes: Vec<String>,
    /// Draft storing default source texts and translated per-language outputs.
    pub draft: BotInfoDraft,
    /// Current job state.
    pub status: TranslateJobStatus,
    /// Optional error banner.
    pub error: Option<String>,
    /// Whether the default (`""`) language has been fetched via `bots.getBotInfo`.
    default_loaded: bool,
}

impl TranslateBotInfoBoxState {
    /// Creates a new [`TranslateBotInfoBoxState`] with initial `codes`.
    #[must_use]
    pub fn new(
        bot_id: PeerId,
        bot_name: impl Into<String>,
        codes: Vec<String>,
        initial_draft: Option<BotInfoDraft>,
    ) -> Self {
        let selected_codes: Vec<String> = codes.into_iter().filter(|c| !c.is_empty()).collect();
        let default_loaded = initial_draft
            .as_ref()
            .is_some_and(|d| d.current.contains_key(""));
        Self {
            bot_id,
            bot_name: bot_name.into(),
            selected_codes,
            pending_codes: Vec::new(),
            draft: initial_draft.unwrap_or_default(),
            status: TranslateJobStatus::Selecting,
            error: None,
            default_loaded,
        }
    }

    /// Toggles `lang_code` in [`Self::selected_codes`].
    pub fn toggle_language(&mut self, lang_code: &str) {
        if lang_code.is_empty() {
            return;
        }
        if let Some(pos) = self.selected_codes.iter().position(|c| c == lang_code) {
            self.selected_codes.remove(pos);
        } else {
            self.selected_codes.push(lang_code.to_owned());
        }
    }

    /// Starts translating all selected target languages from the default language (`""`),
    /// fetching `bots.getBotInfo` if needed and invoking `messages.translateText` via `backend`.
    pub fn start_translate(&mut self, backend: &dyn InfoBackend) -> Result<usize, String> {
        if self.selected_codes.is_empty() {
            let msg = "Select at least one target language.".to_owned();
            self.error = Some(msg.clone());
            return Err(msg);
        }
        if !self.default_loaded {
            let texts = backend.get_bot_info(self.bot_id, "")?;
            self.draft.set_loaded("", texts);
            self.default_loaded = true;
        }
        self.pending_codes = self.selected_codes.clone();
        let total = self.pending_codes.len();
        self.status = TranslateJobStatus::Translating { done: 0, total };
        self.error = None;

        let src = self.draft.current.get("").cloned().unwrap_or_default();
        let inputs = vec![src.name.clone(), src.about.clone(), src.description.clone()];
        let mut translated_count = 0usize;

        while let Some(code) = self.pending_codes.first().cloned() {
            self.pending_codes.remove(0);
            match backend.translate_bot_info_texts(&inputs, &code) {
                Ok(out) => {
                    let target = self.draft.current_mut(&code);
                    if let Some(n) = out.first() {
                        target.name = n.clone();
                    }
                    if let Some(a) = out.get(1) {
                        target.about = a.clone();
                    }
                    if let Some(d) = out.get(2) {
                        target.description = d.clone();
                    }
                    translated_count += 1;
                    self.status = TranslateJobStatus::Translating {
                        done: translated_count,
                        total,
                    };
                }
                Err(err) => {
                    self.status = TranslateJobStatus::Stopped(err.clone());
                    self.error = Some(err.clone());
                    return Err(err);
                }
            }
        }

        self.status = TranslateJobStatus::Finished {
            translated_count,
            saved: false,
        };
        Ok(translated_count)
    }

    /// Cancels an active translation job.
    pub fn stop(&mut self) {
        self.pending_codes.clear();
        if matches!(
            self.status,
            TranslateJobStatus::Translating { .. } | TranslateJobStatus::Paused { .. }
        ) {
            self.status = TranslateJobStatus::Stopped(tr::lng_bot_languages_stopped());
        }
    }

    /// Saves all translated language drafts via `bots.setBotInfo`.
    pub fn save_translations(&mut self, backend: &mut dyn InfoBackend) -> Result<usize, String> {
        let mut saved = 0usize;
        for code in &self.selected_codes {
            let change = self.draft.diff_for(code);
            if change.is_empty() {
                continue;
            }
            backend.set_bot_info(self.bot_id, code, &change)?;
            self.draft.commit_saved(code);
            saved += 1;
        }
        let translated_count = match self.status {
            TranslateJobStatus::Finished {
                translated_count, ..
            } => translated_count,
            _ => saved,
        };
        self.status = TranslateJobStatus::Finished {
            translated_count,
            saved: true,
        };
        self.error = None;
        Ok(saved)
    }

    /// Returns the progress fraction in `0.0..=1.0` for the radial indicator.
    #[must_use]
    pub fn progress_fraction(&self) -> f32 {
        match self.status {
            TranslateJobStatus::Selecting => 0.0,
            TranslateJobStatus::Translating { done, total }
            | TranslateJobStatus::Paused { done, total, .. } => {
                if total == 0 {
                    0.0
                } else {
                    (done as f32 / total as f32).clamp(0.0, 1.0)
                }
            }
            TranslateJobStatus::Stopped(_) => 0.0,
            TranslateJobStatus::Finished { .. } => 1.0,
        }
    }

    /// Returns the localized status string (`StatusText` in `translate_bot_info_box.cpp:88-106`).
    #[must_use]
    pub fn status_text(&self) -> String {
        match &self.status {
            TranslateJobStatus::Selecting => {
                if self.selected_codes.is_empty() {
                    "Choose one or more languages to translate bot info into.".to_owned()
                } else {
                    format!(
                        "Ready to translate {} language(s): {}",
                        self.selected_codes.len(),
                        self.selected_codes.join(", ")
                    )
                }
            }
            TranslateJobStatus::Translating { done, total } => {
                tr::lng_bot_languages_translating(&done.to_string(), &total.to_string())
            }
            TranslateJobStatus::Paused { seconds_left, .. } => {
                tr::lng_bot_languages_paused(&format!("{seconds_left}s"))
            }
            TranslateJobStatus::Stopped(_) => tr::lng_bot_languages_stopped(),
            TranslateJobStatus::Finished {
                translated_count,
                saved,
            } => {
                if *saved {
                    format!("Saved {translated_count} translation(s) to Telegram.")
                } else {
                    tr::lng_bot_languages_translated(*translated_count as i64)
                }
            }
        }
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        let title = match self.status {
            TranslateJobStatus::Selecting => tr::lng_bot_languages_translate_title(),
            _ => tr::lng_bot_languages_translating_title(),
        };
        let mut modal = BoxModal::new(id, title).width(st::box_width.0 as f32);
        match self.status {
            TranslateJobStatus::Selecting => {
                modal = modal
                    .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
                    .add_button(BoxButton::default_btn(
                        "translate_start",
                        tr::lng_bot_languages_translate_button(),
                    ));
            }
            TranslateJobStatus::Translating { .. } | TranslateJobStatus::Paused { .. } => {
                modal = modal.add_button(BoxButton::default_btn(
                    "translate_stop",
                    tr::lng_bot_languages_stop(),
                ));
            }
            TranslateJobStatus::Stopped(_) => {
                modal = modal.add_button(BoxButton::default_btn("close", tr::lng_close()));
            }
            TranslateJobStatus::Finished { saved: false, .. } => {
                modal = modal
                    .add_button(BoxButton::default_btn("close", tr::lng_close()))
                    .add_button(BoxButton::default_btn(
                        "translate_save",
                        tr::lng_settings_save(),
                    ));
            }
            TranslateJobStatus::Finished { saved: true, .. } => {
                modal = modal.add_button(BoxButton::default_btn("close", tr::lng_close()));
            }
        }
        modal
    }

    /// Renders the `TranslateBotInfoBox` body with radial progress ring (`64px`, `3px` stroke).
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) {
        // Centered 64px radial indicator with `st::menu_icon_translate` (`Radial::paintEvent`).
        ui.add_space(8.0);
        let progress = self.progress_fraction();
        let active = matches!(
            self.status,
            TranslateJobStatus::Translating { .. } | TranslateJobStatus::Finished { .. }
        );
        ui.vertical_centered(|ui| {
            let (rect, _) =
                ui.allocate_exact_size(Vec2::splat(TRANSLATE_RADIAL_SIZE), Sense::hover());
            let center = rect.center();
            let radius = (TRANSLATE_RADIAL_SIZE - TRANSLATE_RADIAL_STROKE) * 0.5;
            ui.painter().circle_stroke(
                center,
                radius,
                Stroke::new(TRANSLATE_RADIAL_STROKE, colors.color("windowBgOver")),
            );
            if active && progress > 0.0 {
                paint_radial_arc(
                    ui.painter(),
                    center,
                    radius,
                    progress,
                    TRANSLATE_RADIAL_STROKE,
                    colors.color("windowBgActive"),
                );
            }
            let icon_rect = Rect::from_center_size(center, Vec2::splat(26.0));
            let resolver = ColorsResolver(colors);
            paint_style_icon(
                ui.painter(),
                icon_rect,
                st::menu_icon_translate,
                &resolver,
                Some(colors.color("windowActiveTextFg")),
            );

            ui.add_space(12.0);
            ui.label(
                egui::RichText::new(self.status_text())
                    .font(FontId::new(14.0, family_semibold()))
                    .color(colors.color("windowBoldFg")),
            );
            ui.add_space(6.0);
            FlatLabel::plain(
                &tr::lng_bot_languages_translate_about(),
                FlatLabelStyle::box_label(colors),
            )
            .id_salt("translate_bot_info_about")
            .show(ui);
        });

        ui.add_space(10.0);
        divider_bar(ui, 4.0, colors);
        ui.add_space(6.0);

        // Language picker chips (enabled in Selecting mode, read-only summary during/after job).
        ui.horizontal_wrapped(|ui| {
            let sel_bg = ui.visuals().selection.bg_fill;
            let text_fg = colors.color("windowFg");
            let hover_bg = colors.color("windowBgOver");
            let accent = colors.color("windowActiveTextFg");
            for (code, label) in BOT_INFO_LANGUAGES {
                if code.is_empty() {
                    continue;
                }
                let selected = self.selected_codes.iter().any(|c| c == code);
                let translated =
                    self.draft.current.contains_key(code) && !self.draft.diff_for(code).is_empty();
                let check_painter = |p: &egui::Painter, c: Pos2, sz: f32, col: Color32| {
                    tg_app_core::ui_icons::paint_select_check(p, c, sz, col);
                };
                let resp = tg_app_core::ui_icons::selectable_icon_chip(
                    ui,
                    ("tg_bot_tr_lang", code),
                    selected,
                    label,
                    12.5,
                    text_fg,
                    sel_bg,
                    hover_bg,
                    accent,
                    translated
                        .then_some(&check_painter as &dyn Fn(&egui::Painter, Pos2, f32, Color32)),
                );
                if matches!(self.status, TranslateJobStatus::Selecting) && resp.clicked() {
                    self.toggle_language(code);
                }
            }
        });

        if let Some(ref err) = self.error {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(err)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("boxTextFgError")),
            );
        }
    }
}

fn paint_radial_arc(
    painter: &egui::Painter,
    center: Pos2,
    radius: f32,
    fraction: f32,
    stroke_width: f32,
    color: Color32,
) {
    let steps = ((fraction.clamp(0.0, 1.0) * 48.0).ceil() as usize).max(2);
    let start_angle = -std::f32::consts::FRAC_PI_2;
    let sweep = std::f32::consts::TAU * fraction.clamp(0.0, 1.0);
    let mut points = Vec::with_capacity(steps + 1);
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let angle = start_angle + sweep * t;
        points.push(Pos2::new(
            center.x + radius * angle.cos(),
            center.y + radius * angle.sin(),
        ));
    }
    painter.add(Shape::line(points, Stroke::new(stroke_width, color)));
}

// ============================================================================
// 3. MusicAttachBox (`boxes/music_attach_box.cpp`)
// ============================================================================

/// Saved profile music attachment picker (`boxes/music_attach_box.cpp`).
#[derive(Debug, Clone)]
pub struct MusicAttachBoxState {
    /// Target chat peer where selected audio tracks will be sent.
    pub target_peer_id: PeerId,
    /// Self/owner peer ID whose saved profile music is fetched (`users.getSavedMusic`).
    pub owner_peer_id: PeerId,
    /// Live search query filtering track title and performer.
    pub search_query: String,
    /// Loaded saved profile music tracks.
    pub tracks: Vec<SavedMusicTrack>,
    /// Selected document IDs (order-preserving up to [`MAX_SELECTED_MUSIC_TRACKS`]).
    pub selected_ids: Vec<i64>,
    /// Currently previewing track document ID (if play toggle was clicked).
    pub playing_doc_id: Option<i64>,
    /// Whether initial fetch from `InfoBackend::peer_saved_music` has run.
    pub loaded: bool,
    /// Optional error message.
    pub error: Option<String>,
}

impl MusicAttachBoxState {
    /// Creates a new [`MusicAttachBoxState`] for attaching music to `target_peer_id`.
    #[must_use]
    pub fn new(target_peer_id: PeerId, owner_peer_id: PeerId) -> Self {
        Self {
            target_peer_id,
            owner_peer_id,
            search_query: String::new(),
            tracks: Vec::new(),
            selected_ids: Vec::new(),
            playing_doc_id: None,
            loaded: false,
            error: None,
        }
    }

    /// Loads saved profile music tracks from `backend` (`users.getSavedMusic`) once.
    pub fn ensure_loaded(&mut self, backend: &dyn InfoBackend) {
        if self.loaded {
            return;
        }
        self.reload(backend);
    }

    /// Reloads saved profile music tracks from `backend` (`users.getSavedMusic`).
    pub fn reload(&mut self, backend: &dyn InfoBackend) {
        match backend.peer_saved_music(self.owner_peer_id, 0, 50) {
            Ok(tracks) => {
                self.tracks = tracks;
                self.loaded = true;
                self.error = None;
            }
            Err(err) => {
                self.loaded = true;
                self.error = Some(err);
            }
        }
    }

    /// Toggles selection of `doc_id` up to [`MAX_SELECTED_MUSIC_TRACKS`].
    pub fn toggle_select(&mut self, doc_id: i64) {
        if let Some(pos) = self.selected_ids.iter().position(|&id| id == doc_id) {
            self.selected_ids.remove(pos);
        } else if self.selected_ids.len() < MAX_SELECTED_MUSIC_TRACKS {
            self.selected_ids.push(doc_id);
        }
    }

    /// Returns the currently selected [`SavedMusicTrack`] items in selection order.
    #[must_use]
    pub fn selected_tracks(&self) -> Vec<SavedMusicTrack> {
        let mut out = Vec::with_capacity(self.selected_ids.len());
        for &id in &self.selected_ids {
            if let Some(track) = self.tracks.iter().find(|t| t.document_id == id) {
                out.push(track.clone());
            }
        }
        out
    }

    /// Filters loaded tracks by [`Self::search_query`] (case-insensitive title/performer match).
    #[must_use]
    pub fn filtered_tracks(&self) -> Vec<&SavedMusicTrack> {
        let q = self.search_query.trim().to_lowercase();
        if q.is_empty() {
            return self.tracks.iter().collect();
        }
        let terms: Vec<&str> = q.split_whitespace().collect();
        self.tracks
            .iter()
            .filter(|t| {
                let hay = format!("{} {}", t.title.to_lowercase(), t.performer.to_lowercase());
                terms.iter().all(|term| hay.contains(term))
            })
            .collect()
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        let send_label = if self.selected_ids.is_empty() {
            "Send".to_owned()
        } else {
            format!("Send ({})", self.selected_ids.len())
        };
        BoxModal::new(id, "Attach Music")
            .width(st::box_width.0 as f32 + 24.0)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::default_btn("music_attach_send", send_label))
    }

    /// Renders the `MusicAttachBox` body (search input, Saved Music list, play preview, selection).
    pub fn show_body(&mut self, ui: &mut Ui, backend: &mut dyn InfoBackend, colors: &dyn Colors) {
        self.ensure_loaded(backend);

        let avail_w = ui.available_width().max(220.0);
        InputField::single_line(
            "music_attach_search",
            &mut self.search_query,
            "Search music by title or artist...",
            InputFieldStyle::rounded_search(colors),
        )
        .width(avail_w)
        .show(ui);

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            FlatLabel::plain(
                &tr::lng_settings_saved_music_privacy(),
                FlatLabelStyle::subsection_title(colors),
            )
            .id_salt("music_attach_saved_hdr")
            .show(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !self.selected_ids.is_empty() {
                    ui.label(
                        egui::RichText::new(format!(
                            "{}/{} selected",
                            self.selected_ids.len(),
                            MAX_SELECTED_MUSIC_TRACKS
                        ))
                        .font(FontId::new(12.0, family_semibold()))
                        .color(colors.color("windowActiveTextFg")),
                    );
                }
            });
        });
        ui.add_space(6.0);

        if let Some(ref err) = self.error {
            ui.label(
                egui::RichText::new(err)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("boxTextFgError")),
            );
            ui.add_space(6.0);
        }

        let filtered: Vec<SavedMusicTrack> = self.filtered_tracks().into_iter().cloned().collect();
        if filtered.is_empty() {
            ui.add_space(18.0);
            ui.vertical_centered(|ui| {
                let empty_msg = if self.search_query.trim().is_empty() {
                    "No saved music on your profile yet.".to_owned()
                } else {
                    format!("No tracks found for \"{}\".", self.search_query.trim())
                };
                ui.label(
                    egui::RichText::new(empty_msg)
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("windowSubTextFg")),
                );
            });
            ui.add_space(18.0);
            return;
        }

        let selected_set: BTreeSet<i64> = self.selected_ids.iter().copied().collect();
        let mut toggle_doc: Option<i64> = None;
        let mut toggle_play: Option<i64> = None;

        for track in &filtered {
            let is_selected = selected_set.contains(&track.document_id);
            let is_playing = self.playing_doc_id == Some(track.document_id);
            let row_h = 56.0;
            let (row_rect, row_resp) =
                ui.allocate_exact_size(Vec2::new(avail_w, row_h), Sense::click());
            let row_label = format!("{} - {}", track.title, track.performer);
            row_resp.widget_info(|| {
                egui::WidgetInfo::selected(egui::Role::Button, true, is_selected, row_label.clone())
            });
            if row_resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }

            let row_bg = if is_selected {
                with_alpha(colors.color("windowBgActive"), 0.10)
            } else if row_resp.hovered() {
                colors.color("windowBgOver")
            } else {
                Color32::TRANSPARENT
            };
            if row_bg != Color32::TRANSPARENT {
                ui.painter()
                    .rect_filled(row_rect, CornerRadius::same(8), row_bg);
            }

            // Left 40px circular Play/Pause preview button.
            let play_center = Pos2::new(row_rect.left() + 26.0, row_rect.center().y);
            let play_rect = Rect::from_center_size(play_center, Vec2::splat(40.0));
            let play_resp = ui.interact(
                play_rect,
                ui.scope_id().with(("music_attach_play", track.document_id)),
                Sense::click(),
            );
            if play_resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            let circle_bg = colors.color("windowBgActive");
            ui.painter().circle_filled(play_center, 19.0, circle_bg);
            if is_playing {
                // Pause bars
                let bar_w = 3.5;
                let bar_h = 12.0;
                ui.painter().rect_filled(
                    Rect::from_center_size(
                        Pos2::new(play_center.x - 3.5, play_center.y),
                        Vec2::new(bar_w, bar_h),
                    ),
                    CornerRadius::same(1),
                    Color32::WHITE,
                );
                ui.painter().rect_filled(
                    Rect::from_center_size(
                        Pos2::new(play_center.x + 3.5, play_center.y),
                        Vec2::new(bar_w, bar_h),
                    ),
                    CornerRadius::same(1),
                    Color32::WHITE,
                );
            } else {
                // Play triangle
                let tri = vec![
                    Pos2::new(play_center.x - 4.0, play_center.y - 7.0),
                    Pos2::new(play_center.x - 4.0, play_center.y + 7.0),
                    Pos2::new(play_center.x + 7.0, play_center.y),
                ];
                ui.painter()
                    .add(Shape::convex_polygon(tri, Color32::WHITE, Stroke::NONE));
            }

            // Center two-line title + performer/duration.
            let text_left = row_rect.left() + 56.0;
            let text_right = row_rect.right() - 40.0;
            let text_clip = Rect::from_min_max(
                Pos2::new(text_left, row_rect.top()),
                Pos2::new(text_right.max(text_left + 40.0), row_rect.bottom()),
            );
            let clipped = ui.painter().with_clip_rect(text_clip);
            clipped.text(
                Pos2::new(text_left, row_rect.top() + 12.0),
                Align2::LEFT_TOP,
                &track.title,
                FontId::new(13.5, family_semibold()),
                colors.color("windowBoldFg"),
            );
            let mins = track.duration_secs / 60;
            let secs = track.duration_secs % 60;
            let meta = if is_playing {
                format!("Playing · {} · {mins}:{secs:02}", track.performer)
            } else {
                format!("{} · {mins}:{secs:02}", track.performer)
            };
            clipped.text(
                Pos2::new(text_left, row_rect.top() + 31.0),
                Align2::LEFT_TOP,
                meta,
                FontId::new(12.0, family_regular()),
                if is_playing {
                    colors.color("windowActiveTextFg")
                } else {
                    colors.color("windowSubTextFg")
                },
            );

            // Right multi-select circle check indicator (`22px`).
            let check_center = Pos2::new(row_rect.right() - 20.0, row_rect.center().y);
            if is_selected {
                ui.painter()
                    .circle_filled(check_center, 11.0, colors.color("windowBgActive"));
                let check_pts = vec![
                    Pos2::new(check_center.x - 4.5, check_center.y + 0.5),
                    Pos2::new(check_center.x - 1.2, check_center.y + 3.8),
                    Pos2::new(check_center.x + 5.0, check_center.y - 3.5),
                ];
                ui.painter()
                    .add(Shape::line(check_pts, Stroke::new(2.0, Color32::WHITE)));
            } else {
                ui.painter().circle_stroke(
                    check_center,
                    10.0,
                    Stroke::new(1.5, colors.color("windowSubTextFg")),
                );
            }

            if play_resp.clicked() {
                toggle_play = Some(track.document_id);
            } else if row_resp.clicked() {
                toggle_doc = Some(track.document_id);
            }
        }

        if let Some(doc_id) = toggle_play {
            self.playing_doc_id = if self.playing_doc_id == Some(doc_id) {
                None
            } else {
                Some(doc_id)
            };
        }
        if let Some(doc_id) = toggle_doc {
            self.toggle_select(doc_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::info::mock::MockBackend;

    #[test]
    fn translate_bot_info_box_runs_and_saves() {
        let mut backend = MockBackend::new();
        let bot_id = PeerId(7001);
        backend
            .set_bot_info(
                bot_id,
                "",
                &tg_app_core::core_types::dto::managed_bot::BotInfoChange {
                    name: Some("Helper Bot".to_owned()),
                    about: Some("Short bio".to_owned()),
                    description: Some("Helps automate routine tasks.".to_owned()),
                },
            )
            .expect("seed default bot info");
        let mut state = TranslateBotInfoBoxState::new(
            bot_id,
            "Helper Bot",
            vec!["es".to_owned(), "de".to_owned()],
            None,
        );
        let count = state.start_translate(&backend).expect("translate");
        assert_eq!(count, 2);
        assert_eq!(state.progress_fraction(), 1.0);
        assert!(matches!(
            state.status,
            TranslateJobStatus::Finished {
                translated_count: 2,
                saved: false
            }
        ));
        let saved = state.save_translations(&mut backend).expect("save");
        assert_eq!(saved, 2);
        assert!(matches!(
            state.status,
            TranslateJobStatus::Finished {
                translated_count: 2,
                saved: true
            }
        ));
    }

    #[test]
    fn music_attach_box_loads_filters_and_selects_saved_music() {
        let backend = MockBackend::new();
        let owner_id = PeerId::from_user(1000);
        let mut box_state = MusicAttachBoxState::new(PeerId(201), owner_id);
        box_state.ensure_loaded(&backend);
        assert!(box_state.loaded);
        assert!(!box_state.tracks.is_empty());

        let first_id = box_state.tracks[0].document_id;
        box_state.toggle_select(first_id);
        assert_eq!(box_state.selected_tracks().len(), 1);
        assert_eq!(box_state.selected_tracks()[0].document_id, first_id);

        let first_title = box_state.tracks[0].title.clone();
        box_state.search_query = first_title.chars().take(4).collect();
        assert!(
            box_state
                .filtered_tracks()
                .iter()
                .any(|t| t.document_id == first_id)
        );

        box_state.toggle_select(first_id);
        assert!(box_state.selected_tracks().is_empty());
    }
}
