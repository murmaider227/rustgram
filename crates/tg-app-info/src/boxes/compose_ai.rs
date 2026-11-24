//! TranslateBox modal, Compose with AI modal, Create/Edit AI Tone modal, and Preview AI Tone modal.
//!
//! Port origin:
//! - `Telegram/SourceFiles/boxes/translate_box.cpp`
//! - `Telegram/SourceFiles/boxes/compose_ai_box.cpp`
//! - `Telegram/SourceFiles/boxes/create_ai_tone_box.cpp`
//! - `Telegram/SourceFiles/boxes/preview_ai_tone_box.cpp`
//! - `Telegram/SourceFiles/api/api_compose_with_ai.cpp`
//! - `Telegram/SourceFiles/data/data_ai_compose_tones.cpp`
//!
//! Uses Telegram's server-side Layer-233 RPCs (`messages.composeMessageWithAI`,
//! `messages.translateText`, `aicompose.*`) — never any third-party AI service.

use egui::accesskit::Role;
use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Rect, ScrollArea, Sense, Stroke,
    TextEdit, Ui, Vec2, WidgetInfo,
};
use tg_lang::tr;
use tg_text::TextWithEntities;
use tg_ui::icons::paint_style_icon;
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::Colors;
use tg_ui::tg_style::st;
use tg_ui::theme::ColorsResolver;

use crate::boxes::about_cocoon::{AboutCocoonAction, AboutCocoonBoxState};

trait ModalColorsExt {
    fn window_bg(&self) -> Color32;
    fn window_bg_over(&self) -> Color32;
    fn window_bg_active(&self) -> Color32;
    fn window_fg(&self) -> Color32;
    fn window_bold_fg(&self) -> Color32;
    fn window_sub_text_fg(&self) -> Color32;
    fn window_active_text_fg(&self) -> Color32;
    fn attention_fg(&self) -> Color32;
    fn window_shadow_fg(&self) -> Color32;
    fn window_fg_active(&self) -> Color32;
}

impl<T: Colors + ?Sized> ModalColorsExt for T {
    fn window_bg(&self) -> Color32 {
        self.color("windowBg")
    }
    fn window_bg_over(&self) -> Color32 {
        self.color("windowBgOver")
    }
    fn window_bg_active(&self) -> Color32 {
        self.color("windowBgActive")
    }
    fn window_fg(&self) -> Color32 {
        self.color("windowFg")
    }
    fn window_bold_fg(&self) -> Color32 {
        self.color("windowBoldFg")
    }
    fn window_sub_text_fg(&self) -> Color32 {
        self.color("windowSubTextFg")
    }
    fn window_active_text_fg(&self) -> Color32 {
        self.color("windowActiveTextFg")
    }
    fn attention_fg(&self) -> Color32 {
        self.color("attentionButtonFg")
    }
    fn window_shadow_fg(&self) -> Color32 {
        self.color("windowShadowFg")
    }
    fn window_fg_active(&self) -> Color32 {
        self.color("windowFgActive")
    }
}

use tg_app_core::stand_in::types::{MsgId, PeerId};

// ---------------------------------------------------------------------------
// Constants (matching `compose_ai_box.cpp` & `create_ai_tone_box.cpp`)
// ---------------------------------------------------------------------------

pub use tg_app_core::core_types::dto::compose_ai::*;

/// Action emitted by [`TranslateBoxState::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranslateBoxAction {
    /// Copy translated text to clipboard.
    Copy(String),
    /// User closed the Translate modal.
    Close,
}

/// Stateful message translation modal (`Ui::TranslateBox`).
#[derive(Debug, Clone)]
pub struct TranslateBoxState {
    /// Optional source peer ID (when translating a chat message).
    pub peer_id: Option<PeerId>,
    /// Optional source message ID.
    pub msg_id: Option<MsgId>,
    /// Original message text.
    pub original: TextWithEntities,
    /// Active ISO-639-1 target language code.
    pub target_lang: String,
    /// Optional tone hint (`""`, `"formal"`, `"casual"`).
    pub tone: String,
    /// Translated result text (populated via [`Self::run_translation`]).
    pub translated: Option<TextWithEntities>,
    /// Whether the original text section is expanded.
    pub show_original: bool,
    /// Error message from the most recent RPC attempt.
    pub error: Option<String>,
    /// True until the initial translation request has been executed.
    pub needs_refresh: bool,
}

impl TranslateBoxState {
    /// Creates a new `TranslateBox` modal targeting `target_lang`.
    #[must_use]
    pub fn new(
        peer_id: Option<PeerId>,
        msg_id: Option<MsgId>,
        original: TextWithEntities,
        target_lang: impl Into<String>,
    ) -> Self {
        Self {
            peer_id,
            msg_id,
            original,
            target_lang: target_lang.into(),
            tone: String::new(),
            translated: None,
            show_original: true,
            error: None,
            needs_refresh: true,
        }
    }

    /// Runs `messages.translateText` through `backend` and caches the translated output.
    pub fn run_translation(&mut self, backend: &mut dyn ComposeAiBackend) {
        self.needs_refresh = false;
        let tone_opt = (!self.tone.is_empty()).then_some(self.tone.as_str());
        match backend.translate_text_modal(
            self.peer_id,
            self.msg_id,
            &self.original,
            &self.target_lang,
            tone_opt,
        ) {
            Ok(res) => {
                self.translated = Some(res);
                self.error = None;
            }
            Err(err) => {
                self.error = Some(err);
            }
        }
    }

    /// Builds a `tg-ui` [`BoxModal`] descriptor for `BoxHost`.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        BoxModal::new(id, "Translate Message")
            .width(420.0)
            .add_button(BoxButton::default_btn("close", "Close"))
    }

    /// Renders the `TranslateBox` body inside a modal overlay.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn ComposeAiBackend,
        colors: &dyn Colors,
    ) -> Option<TranslateBoxAction> {
        if self.needs_refresh {
            self.run_translation(backend);
        }

        let mut action = None;
        show_modal_card(ui, "tg_translate_box_modal", 420.0, colors, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Translate")
                        .strong()
                        .size(16.0)
                        .color(colors.window_bold_fg()),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if paint_header_icon_button(
                        ui,
                        "tg_translate_box_close",
                        st::box_title_close_icon,
                        &tr::lng_close(),
                        colors,
                    ) {
                        action = Some(TranslateBoxAction::Close);
                    }
                });
            });
            ui.add_space(8.0);

            // Language & Tone selectors
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new("To:")
                        .size(12.5)
                        .color(colors.window_sub_text_fg()),
                );
                egui::ComboBox::from_id_salt("tg_translate_box_lang")
                    .selected_text(language_display_name(&self.target_lang))
                    .show_ui(ui, |ui| {
                        for &(code, name) in TRANSLATE_TARGET_LANGUAGES {
                            if ui
                                .selectable_label(self.target_lang == code, name)
                                .clicked()
                            {
                                self.target_lang = code.to_owned();
                                self.needs_refresh = true;
                            }
                        }
                    });
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("Tone:")
                        .size(12.5)
                        .color(colors.window_sub_text_fg()),
                );
                for &(code, label) in TRANSLATE_TONES {
                    if ui.selectable_label(self.tone == code, label).clicked() {
                        self.tone = code.to_owned();
                        self.needs_refresh = true;
                    }
                }
            });

            if self.needs_refresh {
                self.run_translation(backend);
            }

            ui.add_space(8.0);
            // Collapsible original message card
            ui.horizontal(|ui| {
                let dir = if self.show_original {
                    tg_app_core::ui_icons::ArrowDir::Down
                } else {
                    tg_app_core::ui_icons::ArrowDir::Right
                };
                if tg_app_core::ui_icons::icon_label_button(
                    ui,
                    "Original",
                    true,
                    move |p, c, s, col| {
                        tg_app_core::ui_icons::paint_triangle(p, c, s * 0.72, dir, col);
                    },
                )
                .clicked()
                {
                    self.show_original = !self.show_original;
                }
            });
            if self.show_original {
                egui::Frame::NONE
                    .fill(colors.window_bg_over())
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ScrollArea::vertical()
                            .id_salt("tg_translate_orig_scroll")
                            .max_height(100.0)
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(&self.original.text)
                                        .size(13.0)
                                        .color(colors.window_fg()),
                                );
                            });
                    });
            }

            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(format!(
                    "Translation ({})",
                    language_display_name(&self.target_lang)
                ))
                .strong()
                .size(12.5)
                .color(colors.window_active_text_fg()),
            );
            ui.add_space(4.0);

            egui::Frame::NONE
                .fill(colors.window_bg_over())
                .corner_radius(CornerRadius::same(8))
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ScrollArea::vertical()
                        .id_salt("tg_translate_res_scroll")
                        .max_height(160.0)
                        .show(ui, |ui| {
                            if let Some(ref err) = self.error {
                                ui.label(
                                    egui::RichText::new(err)
                                        .size(12.5)
                                        .color(colors.attention_fg()),
                                );
                            } else if let Some(ref tr) = self.translated {
                                ui.label(
                                    egui::RichText::new(&tr.text)
                                        .size(13.5)
                                        .color(colors.window_fg()),
                                );
                            } else {
                                ui.label(
                                    egui::RichText::new("Translating...")
                                        .size(12.5)
                                        .color(colors.window_sub_text_fg()),
                                );
                            }
                        });
                });

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if let Some(ref tr) = self.translated
                    && ui.button("Copy Translation").clicked()
                {
                    action = Some(TranslateBoxAction::Copy(tr.text.clone()));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if paint_accent_button(ui, "Close", colors) {
                        action = Some(TranslateBoxAction::Close);
                    }
                });
            });
        });

        if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
            action = Some(TranslateBoxAction::Close);
        }
        action
    }
}

// ---------------------------------------------------------------------------
// 2. Compose with AI Modal (`boxes/compose_ai_box.cpp`)
// ---------------------------------------------------------------------------

/// Active tab of [`ComposeAiBoxState`] matching `ComposeAiMode` in `compose_ai_box.cpp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ComposeAiMode {
    /// Translate message text to another language with optional style.
    Translate,
    /// Rewrite text in a selected built-in, custom, or single-use tone.
    #[default]
    Style,
    /// Proofread / fix grammar and spelling with visual diff highlights.
    Fix,
}

/// Action emitted by [`ComposeAiBoxState::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposeAiBoxAction {
    /// Apply the composed text back into the chat input field.
    Apply(TextWithEntities),
    /// User closed the modal without applying.
    Close,
}

/// Stateful Compose with AI modal (`boxes/compose_ai_box.cpp`) including embedded
/// [`CreateAiToneBoxState`] and [`PreviewAiToneBoxState`] sub-dialogs.
#[derive(Debug, Clone)]
pub struct ComposeAiBoxState {
    /// Original draft text passed from the compose bar.
    pub source_text: TextWithEntities,
    /// Active tab (`Translate`, `Style`, `Fix`).
    pub mode: ComposeAiMode,
    /// Target ISO-639-1 language when in `Translate` mode.
    pub target_lang: String,
    /// Whether `emojify` flag is enabled.
    pub emojify: bool,
    /// Loaded server + custom tones (`aicompose.getTones`).
    pub tones: Vec<AiComposeTone>,
    /// Selected tone index in `tones` (`None` means neutral or single-use custom prompt).
    pub selected_tone_index: Option<usize>,
    /// True when using the one-off custom prompt input (`inputAiComposeToneSingleUse`).
    pub use_single_prompt: bool,
    /// Free-form custom prompt text.
    pub single_prompt: String,
    /// Most recent `messages.composeMessageWithAI` result.
    pub result: Option<ComposeAiResult>,
    /// Most recent RPC error, if any.
    pub error: Option<String>,
    /// Whether tones still need initial loading.
    pub tones_loaded: bool,
    /// Nested Create/Edit AI Tone box when open.
    pub tone_editor: Option<CreateAiToneBoxState>,
    /// Nested Preview AI Tone box when open (`tg://addstyle`).
    pub tone_preview: Option<PreviewAiToneBoxState>,
    /// Nested About Cocoon dialog opened from the header info button
    /// (`boxes/compose_ai_box.cpp:2233,2488`).
    pub about_cocoon: Option<AboutCocoonBoxState>,
}

impl ComposeAiBoxState {
    /// Opens Compose with AI for `source_text`.
    #[must_use]
    pub fn new(source_text: TextWithEntities) -> Self {
        Self {
            source_text,
            mode: ComposeAiMode::Style,
            target_lang: "en".to_owned(),
            emojify: false,
            tones: Vec::new(),
            selected_tone_index: Some(0),
            use_single_prompt: false,
            single_prompt: String::new(),
            result: None,
            error: None,
            tones_loaded: false,
            tone_editor: None,
            tone_preview: None,
            about_cocoon: None,
        }
    }

    /// Returns `true` when the nested `AboutCocoonBox` info modal is currently open.
    #[must_use]
    pub fn is_about_cocoon_open(&self) -> bool {
        self.about_cocoon.is_some()
    }

    /// Loads tones from `backend` if not already loaded.
    pub fn ensure_tones_loaded(&mut self, backend: &mut dyn ComposeAiBackend) {
        if self.tones_loaded {
            return;
        }
        match backend.list_ai_tones() {
            Ok(list) => {
                self.tones = list;
                if self
                    .selected_tone_index
                    .is_some_and(|i| i >= self.tones.len())
                {
                    self.selected_tone_index = (!self.tones.is_empty()).then_some(0);
                }
                self.tones_loaded = true;
            }
            Err(err) => {
                self.error = Some(err);
                self.tones_loaded = true;
            }
        }
    }

    /// Builds the [`ComposeAiRequest`] for the current tab and tone selection.
    #[must_use]
    pub fn build_request(&self) -> ComposeAiRequest {
        let tone = if self.mode == ComposeAiMode::Fix {
            None
        } else if self.use_single_prompt && !self.single_prompt.trim().is_empty() {
            Some(AiComposeToneRef::SingleUsePrompt(
                self.single_prompt.trim().to_owned(),
            ))
        } else {
            self.selected_tone_index
                .and_then(|idx| self.tones.get(idx))
                .map(AiComposeTone::to_ref)
        };

        ComposeAiRequest {
            text: self.source_text.clone(),
            proofread: self.mode == ComposeAiMode::Fix,
            emojify: self.emojify,
            translate_to_lang: (self.mode == ComposeAiMode::Translate)
                .then(|| self.target_lang.clone()),
            tone,
        }
    }

    /// Invokes `messages.composeMessageWithAI` through `backend` and stores the output.
    pub fn run_compose(&mut self, backend: &mut dyn ComposeAiBackend) {
        self.ensure_tones_loaded(backend);
        let req = self.build_request();
        match backend.compose_with_ai(req) {
            Ok(res) => {
                self.result = Some(res);
                self.error = None;
            }
            Err(err) => {
                self.error = Some(err);
            }
        }
    }

    /// Builds a `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        BoxModal::new(id, "Compose with AI")
            .width(470.0)
            .add_button(BoxButton::default_btn("apply", "Apply"))
            .add_button(BoxButton::default_btn("cancel", "Cancel"))
    }

    /// Renders the Compose with AI modal (or its nested Create/Preview Tone sub-modal).
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn ComposeAiBackend,
        colors: &dyn Colors,
    ) -> Option<ComposeAiBoxAction> {
        self.ensure_tones_loaded(backend);

        if let Some(ref mut editor) = self.tone_editor {
            if let Some(ev) = editor.show(ui, backend, colors) {
                match ev {
                    CreateAiToneAction::Saved(saved) => {
                        self.tones_loaded = false;
                        self.ensure_tones_loaded(backend);
                        self.selected_tone_index = self.tones.iter().position(|t| t.id == saved.id);
                        self.use_single_prompt = false;
                        self.run_compose(backend);
                    }
                    CreateAiToneAction::Deleted(_) => {
                        self.tones_loaded = false;
                        self.ensure_tones_loaded(backend);
                    }
                    CreateAiToneAction::Cancelled => {}
                }
                self.tone_editor = None;
            }
            return None;
        }

        if let Some(ref mut preview) = self.tone_preview {
            if let Some(ev) = preview.show(ui, backend, colors) {
                if matches!(
                    ev,
                    PreviewAiToneAction::Saved | PreviewAiToneAction::Removed
                ) {
                    self.tones_loaded = false;
                    self.ensure_tones_loaded(backend);
                }
                self.tone_preview = None;
            }
            return None;
        }

        if let Some(ref mut about) = self.about_cocoon {
            if let Some(ev) = about.show(ui, colors, false) {
                match ev {
                    AboutCocoonAction::OpenLink(url) => {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                    }
                    AboutCocoonAction::Close => {
                        self.about_cocoon = None;
                    }
                }
            }
            return None;
        }

        if self.result.is_none() && self.error.is_none() && !self.source_text.text.trim().is_empty()
        {
            self.run_compose(backend);
        }

        let mut action = None;
        let mut open_about_cocoon = false;
        show_modal_card(ui, "tg_compose_ai_modal", 470.0, colors, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Compose with AI")
                        .strong()
                        .size(16.0)
                        .color(colors.window_bold_fg()),
                );
                // Top-right close and About Cocoon info icon buttons matching
                // `boxes/compose_ai_box.cpp:2230-2235,2485-2490` (`aiComposeBoxClose`,
                // `aiComposeBoxInfoButton` with `lng_sr_ai_compose_info`).
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if paint_header_icon_button(
                        ui,
                        "tg_compose_ai_close",
                        st::box_title_close_icon,
                        &tr::lng_close(),
                        colors,
                    ) {
                        action = Some(ComposeAiBoxAction::Close);
                    }
                    ui.add_space(2.0);
                    if paint_header_icon_button(
                        ui,
                        "tg_compose_ai_info_cocoon",
                        st::menu_icon_info,
                        &tr::lng_sr_ai_compose_info(),
                        colors,
                    ) {
                        open_about_cocoon = true;
                    }
                });
            });
            ui.add_space(6.0);

            // 3 Mode tabs: Translate / Style / Fix
            let mut needs_run = false;
            ui.horizontal_wrapped(|ui| {
                for (tab, label) in [
                    (ComposeAiMode::Translate, "Translate"),
                    (ComposeAiMode::Style, "Style"),
                    (ComposeAiMode::Fix, "Fix Grammar"),
                ] {
                    if ui.selectable_label(self.mode == tab, label).clicked() && self.mode != tab {
                        self.mode = tab;
                        needs_run = true;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.checkbox(&mut self.emojify, "Emojify").changed() {
                        needs_run = true;
                    }
                });
            });
            ui.add_space(6.0);

            if self.mode == ComposeAiMode::Translate {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Target language:")
                            .size(12.0)
                            .color(colors.window_sub_text_fg()),
                    );
                    egui::ComboBox::from_id_salt("tg_compose_ai_lang")
                        .selected_text(language_display_name(&self.target_lang))
                        .show_ui(ui, |ui| {
                            for &(code, name) in TRANSLATE_TARGET_LANGUAGES {
                                if ui
                                    .selectable_label(self.target_lang == code, name)
                                    .clicked()
                                {
                                    self.target_lang = code.to_owned();
                                    needs_run = true;
                                }
                            }
                        });
                });
                ui.add_space(6.0);
            }

            // Tone chips strip (for Style and Translate tabs)
            if self.mode != ComposeAiMode::Fix {
                let mut open_create = false;
                let mut open_edit: Option<AiComposeTone> = None;
                ui.horizontal_wrapped(|ui| {
                    if self.mode == ComposeAiMode::Translate
                        && ui
                            .selectable_label(
                                !self.use_single_prompt && self.selected_tone_index.is_none(),
                                "Neutral",
                            )
                            .clicked()
                    {
                        self.use_single_prompt = false;
                        self.selected_tone_index = None;
                        needs_run = true;
                    }
                    if ui
                        .selectable_label(self.use_single_prompt, "Custom Prompt")
                        .clicked()
                    {
                        self.use_single_prompt = true;
                    }
                    for (idx, tone) in self.tones.iter().enumerate() {
                        let selected =
                            !self.use_single_prompt && self.selected_tone_index == Some(idx);
                        let resp = ui.selectable_label(selected, &tone.title);
                        if resp.clicked() {
                            self.use_single_prompt = false;
                            self.selected_tone_index = Some(idx);
                            needs_run = true;
                        }
                        if !tone.is_default && resp.secondary_clicked() {
                            open_edit = Some(tone.clone());
                        }
                    }
                    if ui.small_button("+ New Style").clicked() {
                        open_create = true;
                    }
                });

                if self.use_single_prompt {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.add(
                            TextEdit::singleline(&mut self.single_prompt)
                                .desired_width(340.0)
                                .hint_text("Describe how to rewrite your message..."),
                        );
                        if ui.button("Generate").clicked() {
                            needs_run = true;
                        }
                    });
                }

                // Edit button for currently selected custom tone
                if !self.use_single_prompt
                    && let Some(sel) = self
                        .selected_tone_index
                        .and_then(|i| self.tones.get(i))
                        .cloned()
                    && !sel.is_default
                {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("Style: {}", sel.prompt))
                                .size(11.0)
                                .color(colors.window_sub_text_fg()),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if sel.creator && ui.small_button("Edit Style").clicked() {
                                open_edit = Some(sel.clone());
                            }
                            if ui.small_button("Preview").clicked() {
                                self.tone_preview =
                                    Some(PreviewAiToneBoxState::new(sel.clone(), true));
                            }
                        });
                    });
                }

                if open_create {
                    self.tone_editor = Some(CreateAiToneBoxState::new_create());
                } else if let Some(tone) = open_edit {
                    self.tone_editor = Some(CreateAiToneBoxState::new_edit(tone));
                }
            }

            if needs_run {
                self.run_compose(backend);
            }

            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("Original")
                    .size(11.5)
                    .color(colors.window_sub_text_fg()),
            );
            egui::Frame::NONE
                .fill(colors.window_bg_over())
                .corner_radius(CornerRadius::same(8))
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        egui::RichText::new(&self.source_text.text)
                            .size(12.5)
                            .color(colors.window_sub_text_fg()),
                    );
                });

            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(match self.mode {
                    ComposeAiMode::Translate => "Translated Result",
                    ComposeAiMode::Style => "Rewritten Result",
                    ComposeAiMode::Fix => "Proofread Result (Diff)",
                })
                .strong()
                .size(12.5)
                .color(colors.window_active_text_fg()),
            );
            egui::Frame::NONE
                .fill(colors.window_bg_over())
                .corner_radius(CornerRadius::same(8))
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ScrollArea::vertical()
                        .id_salt("tg_compose_ai_result_scroll")
                        .max_height(150.0)
                        .show(ui, |ui| {
                            if let Some(ref err) = self.error {
                                ui.label(
                                    egui::RichText::new(err)
                                        .size(12.5)
                                        .color(colors.attention_fg()),
                                );
                            } else if let Some(ref res) = self.result {
                                if self.mode == ComposeAiMode::Fix
                                    && let Some(ref diff) = res.diff
                                    && !diff.entities.is_empty()
                                {
                                    ui.horizontal_wrapped(|ui| {
                                        for seg in build_diff_display(diff) {
                                            match seg {
                                                DiffDisplaySegment::Plain(t) => {
                                                    ui.label(
                                                        egui::RichText::new(t)
                                                            .size(13.0)
                                                            .color(colors.window_fg()),
                                                    );
                                                }
                                                DiffDisplaySegment::Inserted(t) => {
                                                    ui.label(
                                                        egui::RichText::new(t)
                                                            .size(13.0)
                                                            .underline()
                                                            .color(Color32::from_rgb(
                                                                0x2E, 0x9E, 0x5B,
                                                            )),
                                                    );
                                                }
                                                DiffDisplaySegment::Deleted(t) => {
                                                    ui.label(
                                                        egui::RichText::new(t)
                                                            .size(13.0)
                                                            .strikethrough()
                                                            .color(colors.attention_fg()),
                                                    );
                                                }
                                            }
                                        }
                                    });
                                } else {
                                    ui.label(
                                        egui::RichText::new(&res.result_text.text)
                                            .size(13.5)
                                            .color(colors.window_fg()),
                                    );
                                }
                            }
                        });
                });

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Regenerate").clicked() {
                    self.run_compose(backend);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(ref res) = self.result
                        && paint_accent_button(ui, "Apply", colors)
                    {
                        action = Some(ComposeAiBoxAction::Apply(res.result_text.clone()));
                    }
                    ui.add_space(8.0);
                    if ui.button("Cancel").clicked() {
                        action = Some(ComposeAiBoxAction::Close);
                    }
                });
            });
        });

        if open_about_cocoon {
            self.about_cocoon = Some(AboutCocoonBoxState::new());
            return None;
        }

        if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
            action = Some(ComposeAiBoxAction::Close);
        }
        action
    }
}

// ---------------------------------------------------------------------------
// 3. Create / Edit AI Tone Modal (`boxes/create_ai_tone_box.cpp`)
// ---------------------------------------------------------------------------

/// Action emitted by [`CreateAiToneBoxState::show`].
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateAiToneAction {
    /// Custom tone created or updated on Telegram.
    Saved(AiComposeTone),
    /// Existing custom tone deleted (`aicompose.deleteTone`).
    Deleted(i64),
    /// Cancelled.
    Cancelled,
}

/// Stateful Create/Edit AI Tone modal (`boxes/create_ai_tone_box.cpp`).
#[derive(Debug, Clone)]
pub struct CreateAiToneBoxState {
    /// Existing tone when editing (`None` when creating a new tone).
    pub editing: Option<AiComposeTone>,
    /// Tone title (max 32 chars).
    pub title: String,
    /// Prompt instructions (max 500 chars).
    pub prompt: String,
    /// Selected custom emoji document ID.
    pub emoji_id: i64,
    /// Whether to display author attribution on shared tone link.
    pub display_author: bool,
    /// Inline validation or RPC error.
    pub error: Option<String>,
}

impl CreateAiToneBoxState {
    /// Opens an empty Create AI Tone box.
    #[must_use]
    pub fn new_create() -> Self {
        Self {
            editing: None,
            title: String::new(),
            prompt: String::new(),
            emoji_id: AI_TONE_ICON_CHOICES[0].0,
            display_author: true,
            error: None,
        }
    }

    /// Opens an Edit AI Tone box pre-populated from `tone`.
    #[must_use]
    pub fn new_edit(tone: AiComposeTone) -> Self {
        Self {
            title: tone.title.clone(),
            prompt: tone.prompt.clone(),
            emoji_id: if tone.emoji_id != 0 {
                tone.emoji_id
            } else {
                AI_TONE_ICON_CHOICES[0].0
            },
            display_author: tone.author_id != 0,
            editing: Some(tone),
            error: None,
        }
    }

    /// Validates and submits create/update via `backend`.
    pub fn submit(&mut self, backend: &mut dyn ComposeAiBackend) -> Result<AiComposeTone, String> {
        validate_ai_tone_fields(&self.title, &self.prompt)?;
        let res = if let Some(ref existing) = self.editing {
            backend.update_ai_tone(
                existing,
                Some(&self.title),
                Some(&self.prompt),
                Some(self.emoji_id),
                Some(self.display_author),
            )
        } else {
            backend.create_ai_tone(
                &self.title,
                &self.prompt,
                self.emoji_id,
                self.display_author,
            )
        };
        match res {
            Ok(saved) => {
                self.error = None;
                Ok(saved)
            }
            Err(err) => {
                self.error = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Renders the Create/Edit AI Tone modal.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn ComposeAiBackend,
        colors: &dyn Colors,
    ) -> Option<CreateAiToneAction> {
        let mut action = None;
        let is_edit = self.editing.is_some();
        show_modal_card(ui, "tg_create_ai_tone_modal", 410.0, colors, |ui| {
            ui.label(
                egui::RichText::new(if is_edit {
                    "Edit AI Style"
                } else {
                    "New AI Style"
                })
                .strong()
                .size(16.0)
                .color(colors.window_bold_fg()),
            );
            ui.add_space(8.0);

            // Icon selector
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new("Icon:")
                        .size(12.0)
                        .color(colors.window_sub_text_fg()),
                );
                for &(id, glyph) in AI_TONE_ICON_CHOICES {
                    if ui.selectable_label(self.emoji_id == id, glyph).clicked() {
                        self.emoji_id = id;
                    }
                }
            });
            ui.add_space(8.0);

            ui.label(
                egui::RichText::new(format!(
                    "Style Name ({}/{})",
                    self.title.chars().count(),
                    AI_TONE_TITLE_LIMIT
                ))
                .size(12.0)
                .color(colors.window_sub_text_fg()),
            );
            ui.add(
                TextEdit::singleline(&mut self.title)
                    .desired_width(f32::INFINITY)
                    .hint_text("e.g. Concise Release Update"),
            );
            ui.add_space(8.0);

            let prompt_len = self.prompt.chars().count();
            let remaining = AI_TONE_PROMPT_LIMIT as isize - prompt_len as isize;
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Instructions")
                        .size(12.0)
                        .color(colors.window_sub_text_fg()),
                );
                if remaining <= AI_TONE_PROMPT_WARN_THRESHOLD as isize {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(format!("{remaining}"))
                                .size(11.0)
                                .color(if remaining < 0 {
                                    colors.attention_fg()
                                } else {
                                    colors.window_sub_text_fg()
                                }),
                        );
                    });
                }
            });
            ui.add(
                TextEdit::multiline(&mut self.prompt)
                    .desired_width(f32::INFINITY)
                    .desired_rows(4)
                    .hint_text("Describe tone, structure, and wording rules..."),
            );
            ui.add_space(6.0);
            ui.checkbox(
                &mut self.display_author,
                "Display my name on shared style link",
            );

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
                if let Some(ref existing) = self.editing.clone()
                    && ui
                        .button(egui::RichText::new("Delete").color(colors.attention_fg()))
                        .clicked()
                {
                    match backend.delete_ai_tone(existing) {
                        Ok(()) => action = Some(CreateAiToneAction::Deleted(existing.id)),
                        Err(err) => self.error = Some(err),
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if paint_accent_button(ui, if is_edit { "Save" } else { "Create" }, colors)
                        && let Ok(saved) = self.submit(backend)
                    {
                        action = Some(CreateAiToneAction::Saved(saved));
                    }
                    ui.add_space(8.0);
                    if ui.button("Cancel").clicked() {
                        action = Some(CreateAiToneAction::Cancelled);
                    }
                });
            });
        });

        if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
            action = Some(CreateAiToneAction::Cancelled);
        }
        action
    }
}

// ---------------------------------------------------------------------------
// 4. Preview AI Tone Modal (`boxes/preview_ai_tone_box.cpp` / `tg://addstyle`)
// ---------------------------------------------------------------------------

/// Action emitted by [`PreviewAiToneBoxState::show`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewAiToneAction {
    /// Style saved to user's list.
    Saved,
    /// Style removed from user's list.
    Removed,
    /// Closed.
    Closed,
}

/// Preview dialog for a shared AI tone (`tg://addstyle?slug=...`).
#[derive(Debug, Clone)]
pub struct PreviewAiToneBoxState {
    /// Tone being previewed.
    pub tone: AiComposeTone,
    /// Whether this tone is already installed in the user's list.
    pub installed: bool,
    /// Current example index (`aicompose.getToneExample`).
    pub example_num: i32,
    /// Loaded example pair.
    pub example: Option<AiComposeToneExample>,
    /// Error text, if any.
    pub error: Option<String>,
}

impl PreviewAiToneBoxState {
    /// Creates a new preview box for `tone`.
    #[must_use]
    pub fn new(tone: AiComposeTone, installed: bool) -> Self {
        let example = tone.first_example.clone();
        Self {
            tone,
            installed,
            example_num: 0,
            example,
            error: None,
        }
    }

    /// Loads the next example via `aicompose.getToneExample`.
    pub fn load_next_example(&mut self, backend: &mut dyn ComposeAiBackend) {
        self.example_num += 1;
        match backend.get_ai_tone_example(&self.tone, self.example_num) {
            Ok(ex) => {
                self.example = Some(ex);
                self.error = None;
            }
            Err(err) => self.error = Some(err),
        }
    }

    /// Builds a `tg-ui` [`BoxModal`] descriptor for hosting in [`crate::boxes::BoxHost`]
    /// (used when opening `tg://addstyle?slug=...` / `t.me/addstyle/<slug>` deep links).
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        let title = format!("{} {}", self.tone.emoji_label, self.tone.title);
        let primary_label = if self.installed {
            "Remove Style"
        } else {
            "Add Style"
        };
        BoxModal::new(id, title)
            .width(420.0)
            .with_left_button(BoxButton::default_btn("next_example", "Next Example"))
            .add_button(BoxButton::default_btn("cancel", "Close"))
            .add_button(BoxButton::default_btn("toggle_save_tone", primary_label))
    }

    /// Renders the Preview AI Tone body inside a [`crate::boxes::BoxHost`] modal container.
    pub fn show_box_body(&mut self, ui: &mut Ui, colors: &dyn Colors) {
        ui.label(
            egui::RichText::new(format!("{} installs", self.tone.installs_count.max(1)))
                .size(11.5)
                .color(colors.window_sub_text_fg()),
        );
        if !self.tone.prompt.is_empty() {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(&self.tone.prompt)
                    .size(12.5)
                    .color(colors.window_fg()),
            );
        }
        if let Some(ref ex) = self.example {
            ui.add_space(8.0);
            egui::Frame::NONE
                .fill(colors.window_bg_over())
                .corner_radius(CornerRadius::same(8))
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        egui::RichText::new(format!("Before: {}", ex.from.text))
                            .size(12.0)
                            .color(colors.window_sub_text_fg()),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!("After: {}", ex.to.text))
                            .size(12.5)
                            .color(colors.window_fg()),
                    );
                });
        }
        if let Some(ref err) = self.error {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(err)
                    .size(12.0)
                    .color(colors.attention_fg()),
            );
        }
    }

    /// Renders the Preview AI Tone modal.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn ComposeAiBackend,
        colors: &dyn Colors,
    ) -> Option<PreviewAiToneAction> {
        let mut action = None;
        show_modal_card(ui, "tg_preview_ai_tone_modal", 400.0, colors, |ui| {
            ui.label(
                egui::RichText::new(format!("{} {}", self.tone.emoji_label, self.tone.title))
                    .strong()
                    .size(16.0)
                    .color(colors.window_bold_fg()),
            );
            ui.label(
                egui::RichText::new(format!("{} installs", self.tone.installs_count.max(1)))
                    .size(11.5)
                    .color(colors.window_sub_text_fg()),
            );
            if !self.tone.prompt.is_empty() {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(&self.tone.prompt)
                        .size(12.5)
                        .color(colors.window_fg()),
                );
            }
            if let Some(ref ex) = self.example {
                ui.add_space(8.0);
                egui::Frame::NONE
                    .fill(colors.window_bg_over())
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(
                            egui::RichText::new(format!("Before: {}", ex.from.text))
                                .size(12.0)
                                .color(colors.window_sub_text_fg()),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(format!("After: {}", ex.to.text))
                                .size(12.5)
                                .color(colors.window_fg()),
                        );
                    });
            }
            if let Some(ref err) = self.error {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(err)
                        .size(12.0)
                        .color(colors.attention_fg()),
                );
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.small_button("Next Example").clicked() {
                    self.load_next_example(backend);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let btn_label = if self.installed {
                        "Remove Style"
                    } else {
                        "Add Style"
                    };
                    if paint_accent_button(ui, btn_label, colors) {
                        match backend.save_ai_tone(&self.tone, self.installed) {
                            Ok(()) => {
                                action = Some(if self.installed {
                                    PreviewAiToneAction::Removed
                                } else {
                                    PreviewAiToneAction::Saved
                                });
                            }
                            Err(err) => self.error = Some(err),
                        }
                    }
                    ui.add_space(8.0);
                    if ui.button("Close").clicked() {
                        action = Some(PreviewAiToneAction::Closed);
                    }
                });
            });
        });

        if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
            action = Some(PreviewAiToneAction::Closed);
        }
        action
    }
}

// ---------------------------------------------------------------------------
// Shared modal shell helpers
// ---------------------------------------------------------------------------

pub(crate) fn show_modal_card(
    ui: &mut Ui,
    id_salt: &str,
    width: f32,
    colors: &dyn Colors,
    add_contents: impl FnOnce(&mut Ui),
) {
    let screen = ui.ctx().viewport_rect();
    // Keep modal card comfortably inside narrow mobile viewports (e.g. 390x844).
    let clamped_outer_w = width.min((screen.width() - 24.0).max(280.0));
    let pad = if screen.width() < 460.0 { 14 } else { 18 };
    let inner_w = (clamped_outer_w - 2.0 * f32::from(pad)).max(240.0);
    let max_h = (screen.height() - 32.0).max(300.0);

    egui::Area::new(Id::unique((id_salt, "backdrop")))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .show(ui.ctx(), |ui| {
            let (r, _) = ui.allocate_exact_size(screen.size(), Sense::click());
            ui.painter().rect_filled(
                r,
                CornerRadius::ZERO,
                Color32::from_rgba_unmultiplied(0, 0, 0, 115),
            );
        });

    egui::Area::new(Id::unique(id_salt))
        .order(egui::Order::Foreground)
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .show(ui.ctx(), |ui| {
            egui::Frame::NONE
                .fill(colors.window_bg())
                .stroke(Stroke::new(1.0, colors.window_shadow_fg()))
                .corner_radius(CornerRadius::same(12))
                .inner_margin(egui::Margin::same(pad))
                .show(ui, |ui| {
                    ui.set_min_width(inner_w);
                    ui.set_max_width(inner_w);
                    ui.set_max_height(max_h);
                    add_contents(ui);
                });
        });
}

fn paint_header_icon_button(
    ui: &mut Ui,
    id_salt: &str,
    icon: tg_ui::tg_style::Icon,
    accessible_name: &str,
    colors: &dyn Colors,
) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, accessible_name));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .circle_filled(rect.center(), 14.0, colors.window_bg_over());
    }
    if resp.has_focus() {
        ui.painter().circle_stroke(
            rect.center(),
            14.0,
            Stroke::new(1.5, colors.window_active_text_fg()),
        );
    }
    let icon_rect = Rect::from_center_size(rect.center(), Vec2::splat(18.0));
    let tint = if resp.hovered() {
        colors.window_fg()
    } else {
        colors.window_sub_text_fg()
    };
    paint_style_icon(
        ui.painter(),
        icon_rect,
        icon,
        &ColorsResolver(colors),
        Some(tint),
    );
    let _ = id_salt;
    resp.on_hover_text(accessible_name).clicked()
}

pub(crate) fn paint_accent_button(ui: &mut Ui, label: &str, colors: &dyn Colors) -> bool {
    let w = (label.len() as f32 * 8.0 + 28.0).max(76.0);
    let (r, resp) = ui.allocate_exact_size(Vec2::new(w, 30.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    ui.painter()
        .rect_filled(r, CornerRadius::same(6), colors.window_bg_active());
    ui.painter().text(
        r.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(13.0),
        colors.window_fg_active(),
    );
    resp.clicked()
}

/// Helper to create a minimal solid-color preview rectangle for tests/UI badges.
#[allow(dead_code)]
pub(crate) fn badge_rect(rect: Rect) -> Rect {
    rect
}
