//! Interactive showcase and automated screenshot harness for `chat_helpers`.
//!
//! Renders real `eframe`/`egui` frames of [`TabbedPanel`] (Emoji + skin-tone popup, Stickers
//! with live `tg-lottie` previews + Premium-locked markers, GIFs mosaic) and
//! [`FieldAutocompleteView`] (`@mention`, `#hashtag`, `/command`, `:emoji`).

use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use egui::{
    Align, Align2, CentralPanel, Color32, CornerRadius, FontId, Frame, Layout, Margin, Pos2, Rect,
    RichText, Sense, Stroke, TextEdit, Vec2,
};
use tg_emoji::EmojiTextureCache;
use tg_lang::tr;
use tg_ui::fonts::ensure_tdesktop_fonts;

use crate::chat_helpers::backend::ChatHelpersBackend;
use crate::chat_helpers::field_autocomplete::{AutocompleteAction, FieldAutocompleteView};
use crate::chat_helpers::helper_palette_from_theme;
use crate::chat_helpers::mock::MockChatHelpersBackend;
use crate::chat_helpers::strings;
use crate::chat_helpers::tabbed_panel::TabbedPanel;
use crate::chat_helpers::tabbed_selector::{ChatHelpersEvent, SelectorTab};
use tg_app_core::stand_in::theme::{Palette, ThemeMode};

/// Showcase scene preset for interactive inspection or screenshot capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShowcaseScene {
    /// Emoji tab open with the emoji sprite sheets + skin-tone popup on 👍.
    EmojiWithSkinTone,
    /// Stickers tab open showing live `tg-lottie` animations and Premium lock badges.
    StickersLottieAndPremium,
    /// GIFs tab open showing `@gif` search bar, balanced mosaic grid, and emoji category footer.
    GifsMosaic,
    /// Four live [`FieldAutocompleteView`] states (`@mention`, `#hashtag`, `/command`, `:smile`).
    FieldAutocomplete,
}

impl ShowcaseScene {
    /// Default output filename under `crates/tg-app/docs/`.
    #[must_use]
    pub const fn filename(self) -> &'static str {
        match self {
            Self::EmojiWithSkinTone => "chat_helpers_emoji.png",
            Self::StickersLottieAndPremium => "chat_helpers_stickers.png",
            Self::GifsMosaic => "chat_helpers_gifs.png",
            Self::FieldAutocomplete => "chat_helpers_autocomplete.png",
        }
    }
}

/// Interactive `eframe::App` demonstrating all `chat_helpers` widgets wired to a mock compose bar.
pub struct ChatHelpersShowcase {
    backend: MockChatHelpersBackend,
    emoji_cache: EmojiTextureCache,
    panel: TabbedPanel,
    autocomplete: FieldAutocompleteView,
    theme: ThemeMode,
    draft: String,
    messages: Vec<(bool, String, String)>,
    status_note: String,
    scene: ShowcaseScene,
    // Optional sequential screenshot capture queue.
    capture_queue: Vec<(ShowcaseScene, PathBuf)>,
    capture_index: usize,
    capture_frames: u32,
    capture_in_flight: bool,
    captured_count: Arc<AtomicUsize>,
}

impl ChatHelpersShowcase {
    /// Creates a new interactive showcase initialized on `scene`.
    #[must_use]
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        scene: ShowcaseScene,
        capture_queue: Vec<(ShowcaseScene, PathBuf)>,
    ) -> Self {
        let theme = ThemeMode::Day;
        let palette = helper_palette_from_theme(theme == ThemeMode::Night);
        ensure_tdesktop_fonts(&cc.egui_ctx);
        cc.egui_ctx.set_visuals(palette.to_egui_visuals(theme));

        let mut app = Self {
            backend: MockChatHelpersBackend::new(),
            emoji_cache: EmojiTextureCache::new(),
            panel: TabbedPanel::new(),
            autocomplete: FieldAutocompleteView::new(),
            theme,
            draft: String::new(),
            messages: vec![
                (
                    false,
                    "Kestrel".into(),
                    "Welcome to Rustgram! 👋🏽 Inline emoji render crisp at every DPI 🚀".into(),
                ),
                (
                    true,
                    "You".into(),
                    "Testing emoji picker 😄, animated Lottie stickers ⭐, and @gif search 🔥"
                        .into(),
                ),
                (
                    false,
                    "Bramble".into(),
                    "Try typing @kestrel, #rust, /help, or :smile in the compose bar below! 🎉"
                        .into(),
                ),
            ],
            status_note: "Click the smiley button or type @ / # : in the compose bar".into(),
            scene,
            capture_queue,
            capture_index: 0,
            capture_frames: 0,
            capture_in_flight: false,
            captured_count: Arc::new(AtomicUsize::new(0)),
        };
        app.apply_scene(scene);
        app
    }

    /// Configures widgets deterministically for `scene`.
    pub fn apply_scene(&mut self, scene: ShowcaseScene) {
        self.scene = scene;
        self.panel
            .selector_mut()
            .emoji_list_mut()
            .close_skin_tone_popup();
        match scene {
            ShowcaseScene::EmojiWithSkinTone => {
                self.draft = "Looking great! 👍🏽 ".into();
                self.panel.show_immediate();
                self.panel.set_active_tab(SelectorTab::Emoji);
                // Position skin-tone popup right above the 👍 cell in Frequently Used.
                self.panel
                    .selector_mut()
                    .emoji_list_mut()
                    .open_skin_tone_popup(
                        "👍",
                        Rect::from_min_size(Pos2::new(665.0, 290.0), Vec2::new(38.0, 38.0)),
                        false,
                    );
            }
            ShowcaseScene::StickersLottieAndPremium => {
                self.draft = "Check out these stickers ".into();
                self.backend.set_premium(false);
                self.panel.show_immediate();
                self.panel.set_active_tab(SelectorTab::Stickers);
            }
            ShowcaseScene::GifsMosaic => {
                self.draft = String::new();
                self.panel.show_immediate();
                self.panel.set_active_tab(SelectorTab::Gifs);
                self.panel
                    .selector_mut()
                    .gifs_list_mut()
                    .select_category("trending", "trending");
            }
            ShowcaseScene::FieldAutocomplete => {
                self.panel.hide_fast();
                self.draft = "Hello :smi".into();
                self.autocomplete.update_from_input(
                    &self.draft,
                    self.draft.len(),
                    false,
                    &mut self.backend,
                );
            }
        }
    }

    fn show_chat_with_panel(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0.0, palette.chat_canvas_bg);

        // Top header bar (52px)
        let top_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 52.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(top_rect), |ui| {
            ui.painter().rect_filled(top_rect, 0.0, palette.window_bg);
            ui.painter().line_segment(
                [
                    Pos2::new(top_rect.left(), top_rect.bottom()),
                    Pos2::new(top_rect.right(), top_rect.bottom()),
                ],
                Stroke::new(1.0, palette.divider),
            );
            Frame::NONE
                .inner_margin(Margin::symmetric(16, 10))
                .show(ui, |ui| {
                    ui.horizontal_centered(|ui| {
                        let (av_r, _) = ui.allocate_exact_size(Vec2::splat(34.0), Sense::hover());
                        ui.painter().circle_filled(
                            av_r.center(),
                            17.0,
                            Color32::from_rgb(0x40, 0xA7, 0xE3),
                        );
                        ui.painter().text(
                            av_r.center(),
                            Align2::CENTER_CENTER,
                            "TG",
                            FontId::proportional(12.5),
                            Color32::WHITE,
                        );
                        ui.add_space(6.0);
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new(strings::SHOWCASE_ROOM_TITLE)
                                    .size(14.5)
                                    .strong()
                                    .color(palette.window_bold_fg),
                            );
                            let status = format!(
                                "{}, {}",
                                tr::lng_chat_status_members(7),
                                tr::lng_chat_status_online(3)
                            );
                            ui.label(
                                RichText::new(status)
                                    .size(12.0)
                                    .color(palette.window_active_text_fg),
                            );
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(
                                RichText::new(&self.status_note)
                                    .size(11.5)
                                    .color(palette.window_sub_text_fg),
                            );
                        });
                    });
                });
        });

        // Bottom compose bar (54px)
        let compose_rect = Rect::from_min_size(
            Pos2::new(rect.left(), rect.bottom() - 54.0),
            Vec2::new(rect.width(), 54.0),
        );

        // Messages area with inline emoji bubbles
        let msg_rect = Rect::from_min_max(
            Pos2::new(rect.left(), top_rect.bottom()),
            Pos2::new(rect.right(), compose_rect.top()),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(msg_rect), |ui| {
            Frame::NONE
                .inner_margin(Margin::symmetric(20, 16))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 10.0;
                    for (outgoing, sender, body) in &self.messages {
                        let align = if *outgoing { Align::Max } else { Align::Min };
                        ui.with_layout(Layout::top_down(align), |ui| {
                            let fill = if *outgoing {
                                palette.msg_out_bg
                            } else {
                                palette.msg_in_bg
                            };
                            Frame::NONE
                                .fill(fill)
                                .stroke(Stroke::new(0.5, palette.divider))
                                .corner_radius(CornerRadius::same(12))
                                .inner_margin(Margin::symmetric(12, 8))
                                .show(ui, |ui| {
                                    ui.set_max_width(420.0);
                                    if !outgoing {
                                        ui.label(
                                            RichText::new(sender)
                                                .size(12.0)
                                                .strong()
                                                .color(palette.window_active_text_fg),
                                        );
                                    }
                                    // Emoji come from tdesktop's sheets through the galley
                                    // (`tg_emoji::text_glyphs`), like every other text.
                                    ui.set_max_width(396.0);
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(body)
                                                .font(FontId::proportional(14.0))
                                                .color(palette.window_fg),
                                        )
                                        .wrap(),
                                    );
                                });
                        });
                    }
                });
        });

        // Compose bar with Emoji panel toggle button
        let mut emoji_btn_rect = compose_rect;
        ui.scope_builder(egui::UiBuilder::new().max_rect(compose_rect), |ui| {
            ui.painter()
                .rect_filled(compose_rect, 0.0, palette.window_bg);
            ui.painter().line_segment(
                [
                    Pos2::new(compose_rect.left(), compose_rect.top()),
                    Pos2::new(compose_rect.right(), compose_rect.top()),
                ],
                Stroke::new(1.0, palette.divider),
            );
            Frame::NONE
                .inner_margin(Margin::symmetric(14, 9))
                .show(ui, |ui| {
                    ui.horizontal_centered(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let right_controls = 112.0;
                        let input_w = (ui.available_width() - right_controls).max(140.0);
                        let edit = ui.add_sized(
                            Vec2::new(input_w, 34.0),
                            TextEdit::singleline(&mut self.draft).hint_text(tr::lng_message_ph()),
                        );
                        if edit.changed() {
                            self.autocomplete.update_from_input(
                                &self.draft,
                                self.draft.len(),
                                false,
                                &mut self.backend,
                            );
                        }

                        // Emoji / Stickers / GIFs panel toggle button
                        let (btn_r, btn_resp) =
                            ui.allocate_exact_size(Vec2::splat(34.0), Sense::click());
                        emoji_btn_rect = btn_r;
                        if btn_resp.hovered() || self.panel.is_open() {
                            ui.painter().rect_filled(
                                btn_r,
                                CornerRadius::same(17),
                                palette.window_bg_over,
                            );
                        }
                        self.emoji_cache.paint_emoji_str(
                            ui.painter(),
                            ui.ctx(),
                            "😊",
                            btn_r.shrink(5.0),
                            Color32::WHITE,
                        );
                        if btn_resp.clicked() {
                            let now = ui.input(|i| i.time);
                            self.panel.toggle_animated(now);
                        }

                        // Send button
                        let send_btn = ui.add_sized(
                            Vec2::new(62.0, 34.0),
                            egui::Button::new(
                                RichText::new(tr::lng_send_button())
                                    .size(13.5)
                                    .strong()
                                    .color(palette.window_fg_active),
                            )
                            .fill(palette.window_bg_active)
                            .stroke(Stroke::NONE)
                            .corner_radius(CornerRadius::same(6)),
                        );
                        if send_btn.clicked() && !self.draft.trim().is_empty() {
                            let body = std::mem::take(&mut self.draft);
                            self.messages
                                .push((true, strings::SHOWCASE_SELF_SENDER.into(), body));
                        }
                    });
                });
        });

        // Floating TabbedPanel anchored above the emoji button.
        let mut collected = Vec::new();
        self.panel.show(
            ui.ctx(),
            emoji_btn_rect,
            &mut self.backend,
            &mut self.emoji_cache,
            palette,
            |ev| collected.push(ev),
        );
        for ev in collected {
            match ev {
                ChatHelpersEvent::EmojiChosen { emoji } => {
                    self.draft.push_str(&emoji);
                    self.status_note = format!("Inserted emoji {emoji}");
                }
                ChatHelpersEvent::StickerChosen { document_id, .. } => {
                    self.status_note = format!("Sent sticker #{}", document_id.0);
                }
                ChatHelpersEvent::GifChosen { title, .. } => {
                    self.status_note = format!("Sent GIF: {title}");
                }
                ChatHelpersEvent::PremiumRequired { reason, .. } => {
                    self.status_note = reason;
                }
                ChatHelpersEvent::StickerSetInstalled(id) => {
                    self.status_note = format!("Installed pack #{}", id.0);
                }
                ChatHelpersEvent::StickerSetRemoved(id) => {
                    self.status_note = format!("Removed pack #{}", id.0);
                }
                ChatHelpersEvent::RecentStickersCleared => {
                    self.status_note = "Cleared recent stickers".to_owned();
                }
                ChatHelpersEvent::CloseRequested => {}
            }
        }
    }

    fn show_autocomplete_showcase(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0.0, palette.chat_canvas_bg);

        // Title bar
        let hdr = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 48.0));
        ui.painter().rect_filled(hdr, 0.0, palette.window_bg);
        ui.painter().line_segment(
            [
                Pos2::new(hdr.left(), hdr.bottom()),
                Pos2::new(hdr.right(), hdr.bottom()),
            ],
            Stroke::new(1.0, palette.divider),
        );
        ui.painter().text(
            Pos2::new(hdr.left() + 18.0, hdr.center().y),
            Align2::LEFT_CENTER,
            strings::SHOWCASE_AUTOCOMPLETE_TITLE,
            FontId::proportional(15.0),
            palette.window_bold_fg,
        );

        // 2x2 grid showing all 4 autocomplete modes live!
        let cases = strings::SHOWCASE_AUTOCOMPLETE_CASES;
        let margin = 18.0;
        let gap = 18.0;
        let card_w = (rect.width() - margin * 2.0 - gap) * 0.5;
        let card_h = (rect.height() - hdr.height() - margin * 2.0 - gap) * 0.5;

        for (i, &(label, sample_input)) in cases.iter().enumerate() {
            let col = (i % 2) as f32;
            let row = (i / 2) as f32;
            let card = Rect::from_min_size(
                Pos2::new(
                    rect.left() + margin + col * (card_w + gap),
                    hdr.bottom() + margin + row * (card_h + gap),
                ),
                Vec2::new(card_w, card_h),
            );
            ui.painter()
                .rect_filled(card, CornerRadius::same(10), palette.window_bg);
            ui.painter().rect_stroke(
                card,
                CornerRadius::same(10),
                Stroke::new(1.0, palette.divider),
                egui::StrokeKind::Inside,
            );
            ui.painter().text(
                Pos2::new(card.left() + 14.0, card.top() + 16.0),
                Align2::LEFT_CENTER,
                label,
                FontId::proportional(12.5),
                palette.window_sub_text_fg,
            );

            // Mock compose bar at bottom of card
            let bar = Rect::from_min_max(
                Pos2::new(card.left() + 10.0, card.bottom() - 44.0),
                Pos2::new(card.right() - 10.0, card.bottom() - 10.0),
            );
            ui.painter()
                .rect_filled(bar, CornerRadius::same(6), palette.input_bg);
            ui.painter().rect_stroke(
                bar,
                CornerRadius::same(6),
                Stroke::new(1.0, palette.input_border_active),
                egui::StrokeKind::Inside,
            );
            ui.painter().text(
                Pos2::new(bar.left() + 10.0, bar.center().y),
                Align2::LEFT_CENTER,
                format!("{sample_input}|"),
                FontId::proportional(13.5),
                palette.window_fg,
            );

            // Render a real FieldAutocompleteView inline inside this card above `bar`.
            let mut ac = FieldAutocompleteView::new();
            ac.update_from_input(sample_input, sample_input.len(), false, &mut self.backend);
            let popup_h = (card.height() - 88.0).clamp(80.0, 172.0);
            let popup_r = Rect::from_min_max(
                Pos2::new(bar.left(), bar.top() - popup_h - 4.0),
                Pos2::new(bar.right(), bar.top() - 4.0),
            );
            ui.scope_builder(egui::UiBuilder::new().max_rect(popup_r), |ui| {
                Frame::NONE
                    .fill(palette.window_bg)
                    .stroke(Stroke::new(1.0, palette.divider))
                    .corner_radius(CornerRadius::same(8))
                    .show(ui, |ui| {
                        ui.set_min_size(popup_r.size());
                        ui.set_max_size(popup_r.size());
                        paint_static_autocomplete_preview(
                            ui,
                            i,
                            ac.rows(),
                            &mut self.emoji_cache,
                            palette,
                        );
                    });
            });
        }

        // Also ensure unused import stays wired for real interactive mode.
        let _ = AutocompleteAction::SendSticker;
    }
}

fn paint_static_autocomplete_preview(
    ui: &mut egui::Ui,
    card_idx: usize,
    rows: &crate::chat_helpers::field_autocomplete::AutocompleteRows,
    emoji_cache: &mut EmojiTextureCache,
    palette: &Palette,
) {
    use crate::chat_helpers::field_autocomplete::{
        AUTOCOMPLETE_ROW_HEIGHT, AutocompleteRows, format_emoji_suggestion_label,
    };

    egui::ScrollArea::vertical()
        .id_salt(("tg_ac_preview", card_idx))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let w = ui.available_width().max(120.0);
            match rows {
                AutocompleteRows::Mentions { rows, .. } => {
                    for (idx, r) in rows.iter().take(4).enumerate() {
                        let (rect, _) = ui.allocate_exact_size(
                            Vec2::new(w, AUTOCOMPLETE_ROW_HEIGHT),
                            Sense::hover(),
                        );
                        if idx == 0 {
                            ui.painter().rect_filled(rect, 0.0, palette.window_bg_over);
                        }
                        let av = Pos2::new(rect.left() + 22.0, rect.center().y);
                        ui.painter()
                            .circle_filled(av, 14.0, Color32::from_rgb(0x40, 0xA7, 0xE3));
                        let init: String = r.display_name.chars().take(1).collect();
                        ui.painter().text(
                            av,
                            Align2::CENTER_CENTER,
                            init,
                            FontId::proportional(11.5),
                            Color32::WHITE,
                        );
                        ui.painter().text(
                            Pos2::new(rect.left() + 44.0, rect.center().y),
                            Align2::LEFT_CENTER,
                            format!(
                                "{}{}",
                                r.display_name,
                                if r.username.is_empty() {
                                    String::new()
                                } else {
                                    format!("  @{}", r.username)
                                }
                            ),
                            FontId::proportional(13.0),
                            palette.window_fg,
                        );
                    }
                }
                AutocompleteRows::Hashtags { rows, .. } => {
                    for (idx, tag) in rows.iter().take(4).enumerate() {
                        let (rect, _) = ui.allocate_exact_size(
                            Vec2::new(w, AUTOCOMPLETE_ROW_HEIGHT),
                            Sense::hover(),
                        );
                        if idx == 0 {
                            ui.painter().rect_filled(rect, 0.0, palette.window_bg_over);
                        }
                        let av = Pos2::new(rect.left() + 22.0, rect.center().y);
                        ui.painter().circle_filled(av, 12.0, palette.input_bg);
                        ui.painter().text(
                            av,
                            Align2::CENTER_CENTER,
                            "#",
                            FontId::proportional(13.5),
                            palette.window_active_text_fg,
                        );
                        ui.painter().text(
                            Pos2::new(rect.left() + 44.0, rect.center().y),
                            Align2::LEFT_CENTER,
                            format!("#{tag}"),
                            FontId::proportional(13.0),
                            palette.window_fg,
                        );
                    }
                }
                AutocompleteRows::BotCommands { rows, .. } => {
                    for (idx, r) in rows.iter().take(4).enumerate() {
                        let (rect, _) = ui.allocate_exact_size(
                            Vec2::new(w, AUTOCOMPLETE_ROW_HEIGHT),
                            Sense::hover(),
                        );
                        if idx == 0 {
                            ui.painter().rect_filled(rect, 0.0, palette.window_bg_over);
                        }
                        let av = Pos2::new(rect.left() + 22.0, rect.center().y);
                        ui.painter()
                            .circle_filled(av, 13.0, Color32::from_rgb(0x6A, 0x5A, 0xCD));
                        ui.painter().text(
                            av,
                            Align2::CENTER_CENTER,
                            "/",
                            FontId::proportional(13.0),
                            Color32::WHITE,
                        );
                        ui.painter().text(
                            Pos2::new(rect.left() + 44.0, rect.center().y - 6.0),
                            Align2::LEFT_CENTER,
                            r.formatted_command(),
                            FontId::proportional(12.5),
                            palette.window_active_text_fg,
                        );
                        ui.painter().text(
                            Pos2::new(rect.left() + 44.0, rect.center().y + 9.0),
                            Align2::LEFT_CENTER,
                            &r.description,
                            FontId::proportional(11.0),
                            palette.window_sub_text_fg,
                        );
                        if r.ephemeral {
                            let badge = Rect::from_min_size(
                                Pos2::new(rect.right() - 68.0, rect.center().y - 9.0),
                                Vec2::new(60.0, 18.0),
                            );
                            ui.painter().rect_filled(
                                badge,
                                CornerRadius::same(9),
                                palette.input_bg,
                            );
                            ui.painter().text(
                                badge.center(),
                                Align2::CENTER_CENTER,
                                strings::EPHEMERAL_BADGE,
                                FontId::proportional(9.5),
                                palette.window_sub_text_fg,
                            );
                        }
                    }
                }
                AutocompleteRows::Emoji { rows, .. } => {
                    for (idx, item) in rows.iter().take(4).enumerate() {
                        let (rect, _) = ui.allocate_exact_size(
                            Vec2::new(w, AUTOCOMPLETE_ROW_HEIGHT),
                            Sense::hover(),
                        );
                        if idx == 0 {
                            ui.painter().rect_filled(rect, 0.0, palette.window_bg_over);
                        }
                        let ir = Rect::from_center_size(
                            Pos2::new(rect.left() + 24.0, rect.center().y),
                            Vec2::splat(24.0),
                        );
                        emoji_cache.paint_emoji_str(
                            ui.painter(),
                            ui.ctx(),
                            &item.emoji,
                            ir,
                            Color32::WHITE,
                        );
                        let lbl = format_emoji_suggestion_label(item);
                        ui.painter().text(
                            Pos2::new(rect.left() + 48.0, rect.center().y),
                            Align2::LEFT_CENTER,
                            lbl,
                            FontId::proportional(13.0),
                            palette.window_fg,
                        );
                    }
                }
                _ => {}
            }
        });
}

impl eframe::App for ChatHelpersShowcase {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let palette = helper_palette_from_theme(self.theme == ThemeMode::Night);

        CentralPanel::default()
            .frame(Frame::NONE)
            .show(ui, |ui| match self.scene {
                ShowcaseScene::FieldAutocomplete => self.show_autocomplete_showcase(ui, &palette),
                _ => self.show_chat_with_panel(ui, &palette),
            });

        // Automated multi-scene screenshot capture runner (used when `capture_queue` is non-empty).
        if !self.capture_queue.is_empty() {
            if self.capture_index >= self.capture_queue.len() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            if self.captured_count.load(Ordering::Acquire) > self.capture_index {
                self.capture_index += 1;
                self.capture_in_flight = false;
                self.capture_frames = 0;
                if let Some(&(next_scene, _)) = self.capture_queue.get(self.capture_index) {
                    self.apply_scene(next_scene);
                }
                ctx.request_repaint();
                return;
            }
            self.capture_frames += 1;
            if !self.capture_in_flight && self.capture_frames >= 8 {
                self.capture_in_flight = true;
                let path = self.capture_queue[self.capture_index].1.clone();
                let counter = Arc::clone(&self.captured_count);
                let ctx_cb = ctx.clone();
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                    egui::ScreenshotCallback::new(move |img| {
                        #[cfg(test)]
                        save_color_image_png(&img, &path);
                        #[cfg(not(test))]
                        let _ = (&img, &path);
                        counter.fetch_add(1, Ordering::AcqRel);
                        ctx_cb.request_repaint();
                    }),
                ));
            } else {
                ctx.request_repaint();
            }
        }
    }
}

#[cfg(test)]
fn save_color_image_png(img: &egui::ColorImage, path: &std::path::Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let [w, h] = img.size;
    let mut raw = Vec::with_capacity(w * h * 4);
    for px in &img.pixels {
        let [r, g, b, a] = px.to_srgba_unmultiplied();
        raw.extend_from_slice(&[r, g, b, a]);
    }
    if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, raw) {
        let _ = buf.save(path);
    }
}

#[cfg(test)]
mod screenshot_runner {
    use super::*;

    /// Renders all 4 `chat_helpers` screenshots into `crates/tg-app/docs/chat_helpers_*.png`.
    /// Run manually with:
    /// `cargo test -p tg-app capture_chat_helpers_screenshots -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn capture_chat_helpers_screenshots() {
        let docs_dir = tg_app_core::core_types::paths::workspace_app_docs_dir();
        let queue: Vec<(ShowcaseScene, PathBuf)> = [
            ShowcaseScene::EmojiWithSkinTone,
            ShowcaseScene::StickersLottieAndPremium,
            ShowcaseScene::GifsMosaic,
            ShowcaseScene::FieldAutocomplete,
        ]
        .into_iter()
        .map(|s| (s, docs_dir.join(s.filename())))
        .collect();

        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title("Rustgram Chat Helpers Showcase")
                .with_inner_size([840.0, 620.0]),
            #[cfg(target_os = "windows")]
            event_loop_builder: Some(Box::new(|builder| {
                use winit::platform::windows::EventLoopBuilderExtWindows;
                builder.with_any_thread(true);
            })),
            ..Default::default()
        };

        eframe::run_native(
            "Rustgram Chat Helpers Showcase",
            options,
            Box::new(move |cc| {
                Ok(Box::new(ChatHelpersShowcase::new(
                    cc,
                    ShowcaseScene::EmojiWithSkinTone,
                    queue,
                )))
            }),
        )
        .expect("eframe showcase launch");
    }
}
