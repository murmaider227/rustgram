//! Shareable profile/chat QR card theme backdrop selector and peer colors panel.
//!
//! Port origin:
//! - `Telegram/SourceFiles/info/profile/info_profile_share_theme.h` & `.cpp`
//! - `Telegram/SourceFiles/ui/effects/peer_qr_colors_panel.h` & `.cpp`
//! - `Telegram/SourceFiles/ui/effects/peer_qr_surface.h` & `.cpp`
//! - `Telegram/SourceFiles/boxes/boxes.style` (`peerQr*` tokens)

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Margin, Pos2, Rect, Role, Sense,
    Stroke, StrokeKind, TextureHandle, TextureOptions, Ui, Vec2, WidgetInfo,
};
use image::{Rgba, RgbaImage};
use tg_lang::tr;
use tg_theme::Color;
use tg_ui::anim::{SimpleAnimation, Transition, contrast_ratio, with_alpha};
use tg_ui::tg_style::st;
use tg_ui::theme::{Colors, theme_color_to_egui};

use crate::themes::backend::{
    PeerColorOptionRecord, PeerColorsResult, PeerProfileColorOptionRecord, PeerProfileColorsResult,
    ShareThemeBackdropOption, ShareThemeBackdropsResult, ShareThemeChoice, ShareThemeReaction,
    ThemesBackend, UniqueGiftChatThemeRecord,
};
use crate::themes::chat_background::rgba_to_color_image;

/// Maximum floating emoji reactions attached to a share-card theme (`kMaxEmojiKeys` in
/// `info_profile_share_theme.cpp:24`).
pub const MAX_SHARE_THEME_EMOJIS: usize = 6;

/// Selection ring transition duration in ms (`kRingDuration` in `peer_qr_colors_panel.cpp:29`).
const RING_ANIM_DURATION_MS: u32 = 220;
/// Minimum selection ring entrance scale (`kRingMinScale` in `peer_qr_colors_panel.cpp:30`).
const RING_MIN_SCALE: f32 = 0.80;
/// Contrast threshold below which the white selection ring adds a dark hairline
/// (`kHairlineContrast` in `peer_qr_colors_panel.cpp:31`).
const HAIRLINE_CONTRAST_THRESHOLD: f64 = 1.5;
/// Alpha for the contrast-protection hairline (`kHairlineAlpha` in `peer_qr_colors_panel.cpp:32`).
const HAIRLINE_ALPHA: f64 = 0.15;

/// Default share-card backdrop center color (`PeerQrDefaultAppearance()` in `peer_qr_surface.cpp:997`).
pub const DEFAULT_SHARE_CENTER_COLOR: Color = Color::rgb(0x36, 0x37, 0x38);
/// Default share-card backdrop edge color (`PeerQrDefaultAppearance()` in `peer_qr_surface.cpp:997`).
pub const DEFAULT_SHARE_EDGE_COLOR: Color = Color::rgb(0x0E, 0x0F, 0x0F);

/// Active sub-tab inside [`ShareThemePickerState`] matching `lng_qr_box_{colors,emoji,gifts}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShareThemeTab {
    /// Circular gradient backdrop swatches (`PeerQrColorsPanel`).
    #[default]
    Colors,
    /// Floating emoji reactions overlay (up to [`MAX_SHARE_THEME_EMOJIS`]).
    Emoji,
    /// Collectible unique Star Gift share themes (`inputShareThemeGift`).
    Gifts,
}

/// Output events emitted by [`ShareThemePickerState::show`].
#[derive(Debug, Clone, Default)]
pub struct ShareThemePickerOutput {
    /// Dispatched `messages.setShareTheme` choice when the user clicked Apply or Reset.
    pub applied: Option<(i64, ShareThemeChoice)>,
    /// Transient toast notification.
    pub toast: Option<String>,
}

/// Interactive Shareable Theme Backdrop selector (`info_profile_share_theme.cpp` +
/// `peer_qr_colors_panel.cpp`) consuming WP-58 `ThemesBackend` RPCs.
pub struct ShareThemePickerState {
    /// Target peer ID (`self` or managed public channel).
    pub peer_id: i64,
    /// Display title shown in the live share-card preview.
    pub peer_name: String,
    /// Active sub-tab (`Colors`, `Emoji`, `Gifts`).
    pub active_tab: ShareThemeTab,
    /// Available backdrop presets from `messages.getShareThemeBackdrops` (or offline defaults).
    backdrops: Vec<ShareThemeBackdropOption>,
    /// Last `messages.getShareThemeBackdrops` hash (`i32`).
    backdrops_hash: i32,
    /// Whether `get_share_theme_backdrops` has been hydrated from a live backend.
    pub cloud_synced: bool,
    /// Currently selected backdrop ID (`None` = default appearance).
    selected_backdrop_id: Option<i32>,
    /// Selected floating emoji keys (capped at [`MAX_SHARE_THEME_EMOJIS`]).
    selected_emojis: Vec<String>,
    /// Selected collectible gift slug when using `ShareThemeChoice::Gift`.
    selected_gift_slug: Option<String>,
    /// Committed share theme choice for dirty-state tracking.
    committed_choice: ShareThemeChoice,
    /// Peer accent colors from `help.getPeerColors`.
    peer_colors: Vec<PeerColorOptionRecord>,
    /// Suggested peer color IDs from `help.getPeerColors`.
    peer_colors_suggested: Vec<u8>,
    /// Last `help.getPeerColors` hash.
    peer_colors_hash: i32,
    /// Peer profile header colors from `help.getPeerProfileColors`.
    peer_profile_colors: Vec<PeerProfileColorOptionRecord>,
    /// Last `help.getPeerProfileColors` hash.
    peer_profile_colors_hash: i32,
    /// Interruptible 220ms `easeOutCubic` selection ring animation (`PeerQrSelectionRing`).
    ring_anim: SimpleAnimation,
    /// Cached share-card preview texture and its cache key.
    preview_texture: Option<(ShareCardCacheKey, TextureHandle)>,
    /// Monotonic counter incremented whenever the preview texture regenerates.
    preview_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ShareCardCacheKey {
    center: Color,
    edge: Color,
    pattern: Color,
    width: u32,
    height: u32,
}

impl ShareThemePickerState {
    /// Creates a new shareable theme backdrop picker for `(peer_id, peer_name)`.
    #[must_use]
    pub fn new(peer_id: i64, peer_name: impl Into<String>) -> Self {
        Self {
            peer_id,
            peer_name: peer_name.into(),
            active_tab: ShareThemeTab::Colors,
            backdrops: builtin_share_theme_backdrops(),
            backdrops_hash: 0,
            cloud_synced: false,
            selected_backdrop_id: None,
            selected_emojis: Vec::new(),
            selected_gift_slug: None,
            committed_choice: ShareThemeChoice::Default,
            peer_colors: Vec::new(),
            peer_colors_suggested: Vec::new(),
            peer_colors_hash: 0,
            peer_profile_colors: Vec::new(),
            peer_profile_colors_hash: 0,
            ring_anim: SimpleAnimation::new(),
            preview_texture: None,
            preview_generation: 0,
        }
    }

    /// Returns available share-theme backdrop options.
    #[must_use]
    pub fn backdrops(&self) -> &[ShareThemeBackdropOption] {
        &self.backdrops
    }

    /// Returns the last `messages.getShareThemeBackdrops` hash.
    #[must_use]
    pub const fn backdrops_hash(&self) -> i32 {
        self.backdrops_hash
    }

    /// Returns peer accent color options hydrated from `help.getPeerColors`.
    #[must_use]
    pub fn peer_colors(&self) -> &[PeerColorOptionRecord] {
        &self.peer_colors
    }

    /// Returns server-suggested peer accent color IDs.
    #[must_use]
    pub fn peer_colors_suggested(&self) -> &[u8] {
        &self.peer_colors_suggested
    }

    /// Returns peer profile header color options hydrated from `help.getPeerProfileColors`.
    #[must_use]
    pub fn peer_profile_colors(&self) -> &[PeerProfileColorOptionRecord] {
        &self.peer_profile_colors
    }

    /// Returns the currently selected backdrop ID (`None` = Default).
    #[must_use]
    pub const fn selected_backdrop_id(&self) -> Option<i32> {
        self.selected_backdrop_id
    }

    /// Returns the currently selected floating emoji strings.
    #[must_use]
    pub fn selected_emojis(&self) -> &[String] {
        &self.selected_emojis
    }

    /// Returns the live preview texture generation counter (bumped each time the texture updates).
    #[must_use]
    pub const fn preview_generation(&self) -> u64 {
        self.preview_generation
    }

    /// Hydrates backdrops (`messages.getShareThemeBackdrops`), peer colors (`help.getPeerColors`),
    /// and profile colors (`help.getPeerProfileColors`) from `backend`.
    pub fn hydrate_from_backend(&mut self, backend: &dyn ThemesBackend) -> Result<usize, String> {
        let mut errors = Vec::new();

        match backend.get_share_theme_backdrops(self.backdrops_hash) {
            Ok(ShareThemeBackdropsResult::Backdrops { hash, backdrops }) => {
                self.backdrops_hash = hash;
                if !backdrops.is_empty() {
                    self.backdrops = backdrops;
                }
                self.cloud_synced = true;
            }
            Ok(ShareThemeBackdropsResult::NotModified) => {
                self.cloud_synced = true;
            }
            Err(err) => errors.push(format!("getShareThemeBackdrops: {err}")),
        }

        match backend.get_peer_colors(self.peer_colors_hash) {
            Ok(PeerColorsResult::Colors {
                hash,
                options,
                suggested,
            }) => {
                self.peer_colors_hash = hash;
                self.peer_colors = options;
                self.peer_colors_suggested = suggested;
            }
            Ok(PeerColorsResult::NotModified) => {}
            Err(err) => errors.push(format!("getPeerColors: {err}")),
        }

        match backend.get_peer_profile_colors(self.peer_profile_colors_hash) {
            Ok(PeerProfileColorsResult::Colors { hash, options }) => {
                self.peer_profile_colors_hash = hash;
                self.peer_profile_colors = options;
            }
            Ok(PeerProfileColorsResult::NotModified) => {}
            Err(err) => errors.push(format!("getPeerProfileColors: {err}")),
        }

        if errors.is_empty() {
            Ok(self.backdrops.len())
        } else {
            Err(errors.join("; "))
        }
    }

    /// Resets cloud sync caches and restores built-in offline presets (e.g. on logout).
    pub fn reset_cloud_state(&mut self) {
        self.backdrops_hash = 0;
        self.cloud_synced = false;
        self.backdrops = builtin_share_theme_backdrops();
        self.peer_colors.clear();
        self.peer_colors_suggested.clear();
        self.peer_colors_hash = 0;
        self.peer_profile_colors.clear();
        self.peer_profile_colors_hash = 0;
        self.preview_texture = None;
    }

    /// Selects a backdrop by ID (`None` selects the Default backdrop) and starts the 220ms ring animation.
    pub fn select_backdrop(&mut self, backdrop_id: Option<i32>, now_sec: f64) -> bool {
        let valid = match backdrop_id {
            None => true,
            Some(id) => self.backdrops.iter().any(|b| b.backdrop_id == id),
        };
        if !valid {
            return false;
        }
        let changed = self.selected_backdrop_id != backdrop_id || self.selected_gift_slug.is_some();
        self.selected_backdrop_id = backdrop_id;
        self.selected_gift_slug = None;
        if changed {
            self.ring_anim.start(
                now_sec,
                0.0,
                1.0,
                RING_ANIM_DURATION_MS,
                Transition::EaseOutCubic,
            );
        }
        true
    }

    /// Toggles an emoji in the floating emoji reaction list (capped at [`MAX_SHARE_THEME_EMOJIS`]).
    pub fn toggle_emoji(&mut self, emoji: &str) -> bool {
        self.selected_gift_slug = None;
        if let Some(pos) = self.selected_emojis.iter().position(|e| e == emoji) {
            self.selected_emojis.remove(pos);
            true
        } else if self.selected_emojis.len() < MAX_SHARE_THEME_EMOJIS {
            self.selected_emojis.push(emoji.to_owned());
            true
        } else {
            false
        }
    }

    /// Selects a collectible unique Star Gift as the share-card theme (`ShareThemeChoice::Gift`).
    pub fn select_gift(&mut self, slug: impl Into<String>, now_sec: f64) {
        self.selected_gift_slug = Some(slug.into());
        self.ring_anim.start(
            now_sec,
            0.0,
            1.0,
            RING_ANIM_DURATION_MS,
            Transition::EaseOutCubic,
        );
    }

    /// Builds the [`ShareThemeChoice`] payload representing the current selection, matching
    /// `ThemeFromKey` in `info_profile_share_theme.cpp:132-147`.
    #[must_use]
    pub fn current_choice(&self) -> ShareThemeChoice {
        if let Some(ref slug) = self.selected_gift_slug {
            return ShareThemeChoice::Gift {
                msg_id: 0,
                peer_id: None,
                saved_id: 0,
                slug: Some(slug.clone()),
            };
        }
        let default_id = self.default_backdrop_id();
        let effective_backdrop = self
            .selected_backdrop_id
            .filter(|&id| default_id != Some(id));
        let emojis: Vec<ShareThemeReaction> = self
            .selected_emojis
            .iter()
            .take(MAX_SHARE_THEME_EMOJIS)
            .map(|e| ShareThemeReaction::Emoji(e.clone()))
            .collect();
        if effective_backdrop.is_none() && emojis.is_empty() {
            ShareThemeChoice::Default
        } else {
            ShareThemeChoice::EmojiBackdrop {
                backdrop_id: effective_backdrop,
                emojis,
            }
        }
    }

    /// Returns `true` when the active selection differs from the last committed choice.
    #[must_use]
    pub fn has_unapplied_change(&self) -> bool {
        self.current_choice() != self.committed_choice
    }

    /// Commits the current selection via `backend.set_share_theme(peer_id, &choice)` when live.
    pub fn apply_selected(
        &mut self,
        backend: Option<&dyn ThemesBackend>,
    ) -> Result<ShareThemeChoice, String> {
        let choice = self.current_choice();
        if let Some(be) = backend {
            be.set_share_theme(self.peer_id, &choice)?;
        }
        self.committed_choice = choice.clone();
        Ok(choice)
    }

    /// Resets the share theme back to `ShareThemeChoice::Default` and dispatches `set_share_theme`.
    pub fn reset_to_default(
        &mut self,
        backend: Option<&dyn ThemesBackend>,
        now_sec: f64,
    ) -> Result<(), String> {
        self.selected_backdrop_id = None;
        self.selected_emojis.clear();
        self.selected_gift_slug = None;
        self.ring_anim.start(
            now_sec,
            0.0,
            1.0,
            RING_ANIM_DURATION_MS,
            Transition::EaseOutCubic,
        );
        if let Some(be) = backend {
            be.set_share_theme(self.peer_id, &ShareThemeChoice::Default)?;
        }
        self.committed_choice = ShareThemeChoice::Default;
        Ok(())
    }

    /// Resolves `(name, center_color, edge_color, pattern_color, text_color)` for the active preview.
    #[must_use]
    pub fn active_appearance(
        &self,
        gift_themes: &[UniqueGiftChatThemeRecord],
    ) -> (String, Color, Color, Color, Color) {
        if let Some(ref slug) = self.selected_gift_slug
            && let Some(gift) = gift_themes.iter().find(|g| g.slug == *slug)
        {
            let light = gift.theme.settings.get(&tg_theme::CloudThemeType::Light);
            let center = light
                .and_then(|s| s.paper.as_ref())
                .and_then(|p| p.background_colors.first().copied())
                .or_else(|| light.map(|s| s.accent_color))
                .unwrap_or(Color::rgb(0x4A, 0x8F, 0xD6));
            let edge = light
                .and_then(|s| s.paper.as_ref())
                .and_then(|p| p.background_colors.get(1).copied())
                .unwrap_or(Color::rgb(0x1E, 0x3D, 0x6B));
            let title = if gift.num > 0 {
                format!("{} #{}", gift.title, gift.num)
            } else {
                gift.title.clone()
            };
            return (title, center, edge, Color::WHITE, Color::WHITE);
        }
        if let Some(id) = self.selected_backdrop_id
            && let Some(b) = self.backdrops.iter().find(|opt| opt.backdrop_id == id)
        {
            return (
                b.name.clone(),
                b.center_color,
                b.edge_color,
                b.pattern_color,
                b.text_color,
            );
        }
        (
            "Default".to_owned(),
            DEFAULT_SHARE_CENTER_COLOR,
            DEFAULT_SHARE_EDGE_COLOR,
            Color::rgb(0x88, 0x90, 0x98),
            Color::WHITE,
        )
    }

    fn default_backdrop_id(&self) -> Option<i32> {
        self.backdrops
            .iter()
            .find(|b| {
                b.center_color == DEFAULT_SHARE_CENTER_COLOR
                    && b.edge_color == DEFAULT_SHARE_EDGE_COLOR
            })
            .map(|b| b.backdrop_id)
    }

    fn ensure_preview_texture(
        &mut self,
        ctx: &egui::Context,
        center: Color,
        edge: Color,
        pattern: Color,
    ) -> TextureHandle {
        let key = ShareCardCacheKey {
            center,
            edge,
            pattern,
            width: 240,
            height: 220,
        };
        if let Some((ref cached_key, ref tex)) = self.preview_texture
            && *cached_key == key
        {
            return tex.clone();
        }
        let rgba = render_share_card_backdrop(key.width, key.height, center, edge, pattern);
        self.preview_generation = self.preview_generation.wrapping_add(1);
        let tex = ctx.load_texture(
            format!("tg_share_theme_card_{}", self.preview_generation),
            rgba_to_color_image(&rgba),
            TextureOptions::LINEAR,
        );
        self.preview_texture = Some((key, tex.clone()));
        tex
    }

    /// Renders the Shareable Chat/Profile Theme Backdrop selector (`PeerQrColorsPanel` + live card).
    pub fn show(
        &mut self,
        ui: &mut Ui,
        colors: &dyn Colors,
        gift_themes: &[UniqueGiftChatThemeRecord],
        backend: Option<&dyn ThemesBackend>,
    ) -> ShareThemePickerOutput {
        if let Some(be) = backend
            && !self.cloud_synced
        {
            let _ = self.hydrate_from_backend(be);
        }

        let mut out = ShareThemePickerOutput::default();
        let bold_fg = colors.color("windowBoldFg");
        let sub_fg = colors.color("windowSubTextFg");
        let active_bg = colors.color("windowBgActive");
        let accent_fg = colors.color("activeButtonFg");
        let divider = colors.color("shadowFg");
        let bg_over = colors.color("windowBgOver");

        let now_sec = ui.ctx().input(|i| i.time);
        let ring_progress = self.ring_anim.animate_egui(ui.ctx(), 1.0) as f32;

        // Section header
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Shareable Theme Backdrop")
                    .size(15.0)
                    .strong()
                    .color(bold_fg),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let status = if self.cloud_synced {
                    format!("{} backdrops synced", self.backdrops.len())
                } else {
                    "Offline presets".to_owned()
                };
                ui.label(egui::RichText::new(status).size(11.5).color(sub_fg));
            });
        });
        ui.label(
            egui::RichText::new(
                "Customize the radial backdrop, floating emojis, or collectible gift shown on your shareable QR profile card.",
            )
            .size(12.0)
            .color(sub_fg),
        );
        ui.add_space(10.0);

        let side_by_side = ui.available_width() >= 640.0;
        let (title, center_c, edge_c, pattern_c, text_c) = self.active_appearance(gift_themes);
        let preview_tex = self.ensure_preview_texture(ui.ctx(), center_c, edge_c, pattern_c);

        if side_by_side {
            ui.horizontal_top(|ui| {
                self.paint_live_card_preview(ui, &preview_tex, &title, text_c, divider, 260.0);
                ui.add_space(16.0);
                let controls_w = (ui.available_width()).clamp(260.0, 440.0);
                ui.allocate_ui_with_layout(
                    Vec2::new(controls_w, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        self.show_selector_controls(
                            ui,
                            colors,
                            gift_themes,
                            backend,
                            now_sec,
                            ring_progress,
                            bold_fg,
                            sub_fg,
                            active_bg,
                            accent_fg,
                            divider,
                            bg_over,
                            &mut out,
                        );
                    },
                );
            });
        } else {
            let full_w = ui.available_width().clamp(220.0, 360.0);
            self.paint_live_card_preview(ui, &preview_tex, &title, text_c, divider, full_w);
            ui.add_space(10.0);
            self.show_selector_controls(
                ui,
                colors,
                gift_themes,
                backend,
                now_sec,
                ring_progress,
                bold_fg,
                sub_fg,
                active_bg,
                accent_fg,
                divider,
                bg_over,
                &mut out,
            );
        }

        // Peer Accent & Profile Colors strip (hydrated from `help.getPeerColors` / `getPeerProfileColors`)
        if !self.peer_colors.is_empty() || !self.peer_profile_colors.is_empty() {
            ui.add_space(10.0);
            self.paint_peer_colors_strip(ui, bold_fg, sub_fg, divider, bg_over);
        }

        out
    }

    fn paint_live_card_preview(
        &self,
        ui: &mut Ui,
        tex: &TextureHandle,
        backdrop_name: &str,
        text_color: Color,
        divider: Color32,
        card_w: f32,
    ) {
        let card_h = 224.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::new(card_w, card_h), Sense::hover());

        ui.painter().image(
            tex.id(),
            rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(16),
            Stroke::new(1.0, divider),
            StrokeKind::Inside,
        );

        // Floating emoji reactions around the QR tile (`peer_qr_emoji_field.cpp`)
        let emoji_slots = [
            Pos2::new(rect.left() + 26.0, rect.top() + 28.0),
            Pos2::new(rect.right() - 26.0, rect.top() + 30.0),
            Pos2::new(rect.left() + 24.0, rect.center().y),
            Pos2::new(rect.right() - 24.0, rect.center().y + 4.0),
            Pos2::new(rect.left() + 34.0, rect.bottom() - 48.0),
            Pos2::new(rect.right() - 34.0, rect.bottom() - 48.0),
        ];
        for (idx, emoji) in self.selected_emojis.iter().take(6).enumerate() {
            ui.painter().text(
                emoji_slots[idx],
                Align2::CENTER_CENTER,
                emoji,
                FontId::proportional(18.0),
                Color32::WHITE,
            );
        }

        // Centered white QR card matching `peerQrCardWidth` & `kCardRadiusRatio = 0.125`
        let inner_side = 114.0;
        let qr_card = Rect::from_center_size(
            Pos2::new(rect.center().x, rect.top() + 88.0),
            Vec2::splat(inner_side),
        );
        ui.painter().rect_filled(
            qr_card.translate(Vec2::new(0.0, 4.0)),
            CornerRadius::same(14),
            Color32::from_rgba_unmultiplied(0, 0, 0, 40),
        );
        ui.painter()
            .rect_filled(qr_card, CornerRadius::same(14), Color32::WHITE);

        // Crisp stylized QR finder modules inside the card
        let ink = Color32::from_rgb(0x18, 0x22, 0x2D);
        let pad = 14.0;
        let qr_area = qr_card.shrink(pad);
        let finder = 22.0;
        for pos in [
            qr_area.min,
            Pos2::new(qr_area.right() - finder, qr_area.top()),
            Pos2::new(qr_area.left(), qr_area.bottom() - finder),
        ] {
            let frect = Rect::from_min_size(pos, Vec2::splat(finder));
            ui.painter().rect_stroke(
                frect,
                CornerRadius::same(4),
                Stroke::new(3.0, ink),
                StrokeKind::Inside,
            );
            ui.painter()
                .rect_filled(frect.shrink(6.0), CornerRadius::same(2), ink);
        }
        // Deterministic inner QR dots
        for &(gx, gy) in &[
            (0.45f32, 0.25f32),
            (0.55, 0.45),
            (0.35, 0.55),
            (0.65, 0.65),
            (0.50, 0.78),
            (0.75, 0.48),
            (0.78, 0.78),
        ] {
            let dot = Rect::from_center_size(
                Pos2::new(
                    qr_area.left() + qr_area.width() * gx,
                    qr_area.top() + qr_area.height() * gy,
                ),
                Vec2::splat(7.0),
            );
            ui.painter().rect_filled(dot, CornerRadius::same(2), ink);
        }

        // Peer username + backdrop name caption with guaranteed readable foreground
        let fg = theme_color_to_egui(text_color);
        let caption_fg = if contrast_ratio(fg, Color32::from_rgb(0x16, 0x18, 0x1C)) < 3.0 {
            Color32::WHITE
        } else {
            fg
        };
        ui.painter().text(
            Pos2::new(rect.center().x, qr_card.bottom() + 20.0),
            Align2::CENTER_CENTER,
            format!("@{}", self.peer_name.to_ascii_lowercase().replace(' ', "_")),
            FontId::proportional(14.0),
            caption_fg,
        );
        ui.painter().text(
            Pos2::new(rect.center().x, qr_card.bottom() + 40.0),
            Align2::CENTER_CENTER,
            backdrop_name,
            FontId::proportional(11.5),
            with_alpha(caption_fg, 0.85),
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn show_selector_controls(
        &mut self,
        ui: &mut Ui,
        colors: &dyn Colors,
        gift_themes: &[UniqueGiftChatThemeRecord],
        backend: Option<&dyn ThemesBackend>,
        now_sec: f64,
        ring_progress: f32,
        bold_fg: Color32,
        sub_fg: Color32,
        active_bg: Color32,
        accent_fg: Color32,
        divider: Color32,
        bg_over: Color32,
        out: &mut ShareThemePickerOutput,
    ) {
        let colors_lbl = tr::lng_qr_box_colors();
        let emoji_lbl = tr::lng_qr_box_emoji();
        let gifts_lbl = tr::lng_qr_box_gifts();

        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            for (tab, label) in [
                (ShareThemeTab::Colors, colors_lbl.as_str()),
                (ShareThemeTab::Emoji, emoji_lbl.as_str()),
                (ShareThemeTab::Gifts, gifts_lbl.as_str()),
            ] {
                if ui.selectable_label(self.active_tab == tab, label).clicked() {
                    self.active_tab = tab;
                }
            }
        });
        ui.add_space(8.0);

        let panel_pad = (st::peer_qr_panel_padding.scaled().max(8) as i8).clamp(8, 16);
        let panel_radius = (st::peer_qr_panel_radius.scaled().clamp(10, 16)) as u8;

        Frame::NONE
            .fill(bg_over)
            .corner_radius(CornerRadius::same(panel_radius))
            .inner_margin(Margin::same(panel_pad))
            .show(ui, |ui| match self.active_tab {
                ShareThemeTab::Colors => {
                    self.show_colors_grid(ui, now_sec, ring_progress, active_bg, sub_fg);
                }
                ShareThemeTab::Emoji => {
                    self.show_emoji_selector(ui, bold_fg, sub_fg, active_bg, divider);
                }
                ShareThemeTab::Gifts => {
                    self.show_gift_selector(
                        ui,
                        gift_themes,
                        now_sec,
                        bold_fg,
                        sub_fg,
                        active_bg,
                        divider,
                    );
                }
            });

        ui.add_space(10.0);

        // Apply / Reset action bar (`SaveQrTheme` in `info_profile_share_theme.cpp`)
        let _ = colors;
        ui.horizontal_wrapped(|ui| {
            let changed = self.has_unapplied_change();
            let apply_btn = egui::Button::new(
                egui::RichText::new("Apply Share Theme")
                    .color(accent_fg)
                    .strong(),
            )
            .fill(active_bg)
            .min_size(Vec2::new(142.0, 32.0));
            if ui.add_enabled(changed, apply_btn).clicked() {
                match self.apply_selected(backend) {
                    Ok(choice) => {
                        out.applied = Some((self.peer_id, choice));
                        out.toast = Some(format!("Share theme updated for {}", self.peer_name));
                    }
                    Err(err) => {
                        out.toast = Some(format!(
                            "Could not update share theme ({err}). Check your connection and try again."
                        ));
                    }
                }
            }

            let reset_label = tr::lng_background_reset_default();
            if ui
                .add_enabled(
                    self.current_choice() != ShareThemeChoice::Default
                        || self.committed_choice != ShareThemeChoice::Default,
                    egui::Button::new(reset_label).min_size(Vec2::new(78.0, 32.0)),
                )
                .clicked()
            {
                match self.reset_to_default(backend, now_sec) {
                    Ok(()) => {
                        out.applied = Some((self.peer_id, ShareThemeChoice::Default));
                        out.toast = Some(format!("Share theme reset for {}", self.peer_name));
                    }
                    Err(err) => {
                        out.toast = Some(format!(
                            "Could not reset share theme ({err}). Try again."
                        ));
                    }
                }
            }
        });
    }

    fn show_colors_grid(
        &mut self,
        ui: &mut Ui,
        now_sec: f64,
        ring_progress: f32,
        focus_color: Color32,
        sub_fg: Color32,
    ) {
        let gap = st::peer_qr_swatch_skip.scaled().max(8) as f32;
        let ring_width = st::peer_qr_swatch_ring.scaled().max(2) as f32;
        let ring_inset = st::peer_qr_swatch_ring_inset.scaled().max(4) as f32;
        let avail = ui.available_width().max(180.0);
        // `kColumns = 6` on desktop, adapting down to 4 on narrow 390px viewports so swatches stay >= 40px.
        let cols: usize = if avail < 290.0 { 4 } else { 6 };
        let circle_side = ((avail - gap * (cols as f32 - 1.0)) / cols as f32).clamp(36.0, 52.0);

        let default_id = self.default_backdrop_id();
        let mut entries: Vec<(Option<i32>, String, Color, Color)> = Vec::new();
        if default_id.is_none() {
            entries.push((
                None,
                "Default Backdrop".to_owned(),
                DEFAULT_SHARE_CENTER_COLOR,
                DEFAULT_SHARE_EDGE_COLOR,
            ));
        }
        for b in &self.backdrops {
            let opt_id = if default_id == Some(b.backdrop_id) {
                None
            } else {
                Some(b.backdrop_id)
            };
            entries.push((opt_id, b.name.clone(), b.center_color, b.edge_color));
        }

        let mut picked: Option<Option<i32>> = None;
        for row in entries.chunks(cols) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for (opt_id, name, center_c, edge_c) in row {
                    let is_selected =
                        self.selected_gift_slug.is_none() && self.selected_backdrop_id == *opt_id;
                    let (rect, resp) =
                        ui.allocate_exact_size(Vec2::splat(circle_side), Sense::click());
                    let aria_label = format!("Share backdrop: {name}");
                    resp.widget_info(|| {
                        WidgetInfo::selected(Role::Button, true, is_selected, aria_label.clone())
                    });
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }

                    let radius = circle_side * 0.5;
                    let center = rect.center();
                    let edge_col = theme_color_to_egui(*edge_c);
                    let center_col = theme_color_to_egui(*center_c);

                    // Layered circle simulating smooth diagonal/radial gradient swatch
                    ui.painter().circle_filled(center, radius, edge_col);
                    ui.painter().circle_filled(
                        Pos2::new(center.x - radius * 0.14, center.y - radius * 0.14),
                        radius * 0.72,
                        center_col,
                    );

                    // Animated `PeerQrSelectionRing` (`peer_qr_colors_panel.cpp:211-260`)
                    if is_selected {
                        let scale = RING_MIN_SCALE + (1.0 - RING_MIN_SCALE) * ring_progress;
                        let r_ring = (radius - ring_inset).max(6.0) * scale;
                        let needs_hairline = contrast_ratio(Color32::WHITE, edge_col)
                            < HAIRLINE_CONTRAST_THRESHOLD
                            || contrast_ratio(Color32::WHITE, center_col)
                                < HAIRLINE_CONTRAST_THRESHOLD;
                        if needs_hairline {
                            ui.painter().circle_stroke(
                                center,
                                r_ring,
                                Stroke::new(
                                    ring_width * 2.0,
                                    with_alpha(
                                        Color32::BLACK,
                                        HAIRLINE_ALPHA * f64::from(ring_progress),
                                    ),
                                ),
                            );
                        }
                        ui.painter().circle_stroke(
                            center,
                            r_ring,
                            Stroke::new(
                                ring_width,
                                with_alpha(Color32::WHITE, f64::from(ring_progress)),
                            ),
                        );
                    }

                    // Visible keyboard focus ring (WCAG 2.4.7 / 1.4.11)
                    if resp.has_focus() {
                        ui.painter().circle_stroke(
                            center,
                            radius + 2.0,
                            Stroke::new(2.0, focus_color),
                        );
                    }

                    let activated = resp.clicked()
                        || (resp.has_focus()
                            && ui.input(|i| {
                                i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Space)
                            }));
                    if activated {
                        picked = Some(*opt_id);
                    }
                    resp.on_hover_text(name);
                }
            });
            ui.add_space(gap * 0.8);
        }

        if let Some(choice) = picked {
            self.select_backdrop(choice, now_sec);
            ui.ctx().request_repaint();
        }

        let _ = sub_fg;
    }

    fn show_emoji_selector(
        &mut self,
        ui: &mut Ui,
        bold_fg: Color32,
        sub_fg: Color32,
        active_bg: Color32,
        divider: Color32,
    ) {
        ui.label(
            egui::RichText::new(format!(
                "Floating Emojis ({}/{MAX_SHARE_THEME_EMOJIS})",
                self.selected_emojis.len()
            ))
            .size(12.0)
            .strong()
            .color(bold_fg),
        );
        ui.add_space(6.0);

        const PRESET_EMOJIS: &[&str] = &[
            "✨", "🔥", "💎", "🚀", "🌊", "🌿", "⚡", "🌙", "⭐", "🎨", "🎵", "❄️",
        ];
        let mut toggled: Option<String> = None;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
            for &emoji in PRESET_EMOJIS {
                let is_on = self.selected_emojis.iter().any(|e| e == emoji);
                let (rect, resp) = ui.allocate_exact_size(Vec2::splat(36.0), Sense::click());
                let aria = format!("Share emoji {emoji}");
                resp.widget_info(|| WidgetInfo::selected(Role::Button, true, is_on, aria.clone()));
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                ui.painter().rect_stroke(
                    rect,
                    CornerRadius::same(10),
                    Stroke::new(
                        if is_on || resp.has_focus() { 2.0 } else { 1.0 },
                        if is_on || resp.has_focus() {
                            active_bg
                        } else {
                            divider
                        },
                    ),
                    StrokeKind::Inside,
                );
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    emoji,
                    FontId::proportional(17.0),
                    bold_fg,
                );
                let activated = resp.clicked()
                    || (resp.has_focus()
                        && ui.input(|i| {
                            i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Space)
                        }));
                if activated {
                    toggled = Some(emoji.to_owned());
                }
            }
        });
        if let Some(emoji) = toggled {
            self.toggle_emoji(&emoji);
        }
        if !self.selected_emojis.is_empty() {
            ui.add_space(6.0);
            if ui.small_button("Clear Emojis").clicked() {
                self.selected_emojis.clear();
            }
        }
        let _ = sub_fg;
    }

    #[allow(clippy::too_many_arguments)]
    fn show_gift_selector(
        &mut self,
        ui: &mut Ui,
        gift_themes: &[UniqueGiftChatThemeRecord],
        now_sec: f64,
        bold_fg: Color32,
        sub_fg: Color32,
        active_bg: Color32,
        divider: Color32,
    ) {
        if gift_themes.is_empty() {
            ui.label(
                egui::RichText::new(tr::lng_qr_box_gifts_empty_title())
                    .size(13.0)
                    .strong()
                    .color(bold_fg),
            );
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(tr::lng_qr_box_gifts_empty_about())
                    .size(12.0)
                    .color(sub_fg),
            );
            return;
        }

        let card_radius = st::peer_qr_gift_radius.scaled().clamp(8, 14) as u8;
        let mut picked_slug: Option<String> = None;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
            for gift in gift_themes {
                let is_selected = self.selected_gift_slug.as_deref() == Some(gift.slug.as_str());
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(112.0, 62.0), Sense::click());
                let label = format!("Share gift theme: {} #{}", gift.title, gift.num);
                resp.widget_info(|| {
                    WidgetInfo::selected(Role::Button, true, is_selected, label.clone())
                });
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                let accent = gift
                    .theme
                    .settings
                    .get(&tg_theme::CloudThemeType::Light)
                    .map(|s| theme_color_to_egui(s.accent_color))
                    .unwrap_or(active_bg);
                ui.painter().rect_filled(
                    rect,
                    CornerRadius::same(card_radius),
                    with_alpha(accent, 0.22),
                );
                ui.painter().rect_stroke(
                    rect,
                    CornerRadius::same(card_radius),
                    Stroke::new(
                        if is_selected || resp.has_focus() {
                            2.0
                        } else {
                            1.0
                        },
                        if is_selected || resp.has_focus() {
                            active_bg
                        } else {
                            divider
                        },
                    ),
                    StrokeKind::Inside,
                );
                ui.painter().text(
                    Pos2::new(rect.left() + 10.0, rect.top() + 18.0),
                    Align2::LEFT_CENTER,
                    &gift.title,
                    FontId::proportional(12.0),
                    bold_fg,
                );
                ui.painter().text(
                    Pos2::new(rect.left() + 10.0, rect.top() + 38.0),
                    Align2::LEFT_CENTER,
                    format!("#{}", gift.num),
                    FontId::proportional(11.0),
                    sub_fg,
                );
                let activated = resp.clicked()
                    || (resp.has_focus()
                        && ui.input(|i| {
                            i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Space)
                        }));
                if activated {
                    picked_slug = Some(gift.slug.clone());
                }
            }
        });
        if let Some(slug) = picked_slug {
            self.select_gift(slug, now_sec);
        }
    }

    fn paint_peer_colors_strip(
        &self,
        ui: &mut Ui,
        bold_fg: Color32,
        sub_fg: Color32,
        divider: Color32,
        bg_over: Color32,
    ) {
        Frame::NONE
            .fill(bg_over)
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new("Peer Accent & Profile Colors")
                            .size(12.5)
                            .strong()
                            .color(bold_fg),
                    );
                    ui.label(
                        egui::RichText::new(format!(
                            "({} accent, {} profile)",
                            self.peer_colors.len(),
                            self.peer_profile_colors.len()
                        ))
                        .size(11.5)
                        .color(sub_fg),
                    );
                });
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                    for opt in self.peer_colors.iter().take(12) {
                        if let Some(&c) = opt.light_colors.first() {
                            let (r, _) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::hover());
                            ui.painter()
                                .circle_filled(r.center(), 9.0, theme_color_to_egui(c));
                            ui.painter()
                                .circle_stroke(r.center(), 9.0, Stroke::new(1.0, divider));
                        }
                    }
                    for opt in self.peer_profile_colors.iter().take(8) {
                        if let Some(&c) = opt.light.bg.first().or_else(|| opt.light.palette.first())
                        {
                            let (r, _) =
                                ui.allocate_exact_size(Vec2::new(26.0, 18.0), Sense::hover());
                            ui.painter().rect_filled(
                                r,
                                CornerRadius::same(6),
                                theme_color_to_egui(c),
                            );
                            ui.painter().rect_stroke(
                                r,
                                CornerRadius::same(6),
                                Stroke::new(1.0, divider),
                                StrokeKind::Inside,
                            );
                        }
                    }
                });
            });
    }
}

/// Renders a smooth radial share-card backdrop (`center_color` -> `edge_color`) with subtle
/// deterministic pattern flecks tinted by `pattern_color`.
#[must_use]
pub fn render_share_card_backdrop(
    width: u32,
    height: u32,
    center: Color,
    edge: Color,
    pattern: Color,
) -> RgbaImage {
    let w = width.max(1);
    let h = height.max(1);
    let cx = (w as f32) * 0.5;
    let cy = (h as f32) * 0.42;
    let max_r = (cx.hypot(cy)).max(1.0) * 1.15;
    let mut img = RgbaImage::new(w, h);

    let corner_r = 16.0f32.min((w.min(h) as f32) * 0.25);

    for y in 0..h {
        let dy = (y as f32) - cy;
        for x in 0..w {
            let dx = (x as f32) - cx;
            let t = (dx.hypot(dy) / max_r).clamp(0.0, 1.0);
            // Smooth quadratic radial falloff matching `PeerQrSurface`
            let s = t * t * (3.0 - 2.0 * t);
            let mut r = f32::from(center.r) + (f32::from(edge.r) - f32::from(center.r)) * s;
            let mut g = f32::from(center.g) + (f32::from(edge.g) - f32::from(center.g)) * s;
            let mut b = f32::from(center.b) + (f32::from(edge.b) - f32::from(center.b)) * s;

            // Subtle geometric pattern dots tinted by `pattern`
            let gx = (x as i32).rem_euclid(28) - 14;
            let gy = (y as i32).rem_euclid(28) - 14;
            if gx * gx + gy * gy <= 6 {
                let pa = 0.14;
                r = r * (1.0 - pa) + f32::from(pattern.r) * pa;
                g = g * (1.0 - pa) + f32::from(pattern.g) * pa;
                b = b * (1.0 - pa) + f32::from(pattern.b) * pa;
            }

            // Antialiased rounded-rectangle alpha mask matching the 16px card border
            let px = (x as f32) + 0.5;
            let py = (y as f32) + 0.5;
            let qx = (px - (w as f32) * 0.5).abs() - ((w as f32) * 0.5 - corner_r);
            let qy = (py - (h as f32) * 0.5).abs() - ((h as f32) * 0.5 - corner_r);
            let dist = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - corner_r;
            let alpha = ((0.5 - dist).clamp(0.0, 1.0) * 255.0).round() as u8;

            img.put_pixel(
                x,
                y,
                Rgba([
                    r.round().clamp(0.0, 255.0) as u8,
                    g.round().clamp(0.0, 255.0) as u8,
                    b.round().clamp(0.0, 255.0) as u8,
                    alpha,
                ]),
            );
        }
    }
    img
}

/// Built-in offline fallback share-card backdrop presets replaced by live
/// `messages.getShareThemeBackdrops` once connected.
#[must_use]
pub fn builtin_share_theme_backdrops() -> Vec<ShareThemeBackdropOption> {
    vec![
        ShareThemeBackdropOption {
            backdrop_id: 1,
            name: "Azure Sky".to_owned(),
            center_color: Color::rgb(0x45, 0xA8, 0xE8),
            edge_color: Color::rgb(0x1E, 0x5B, 0xA8),
            pattern_color: Color::rgb(0x98, 0xD8, 0xFF),
            text_color: Color::WHITE,
        },
        ShareThemeBackdropOption {
            backdrop_id: 2,
            name: "Emerald Mist".to_owned(),
            center_color: Color::rgb(0x3E, 0xB8, 0x85),
            edge_color: Color::rgb(0x16, 0x66, 0x4E),
            pattern_color: Color::rgb(0x9C, 0xEE, 0xC8),
            text_color: Color::WHITE,
        },
        ShareThemeBackdropOption {
            backdrop_id: 3,
            name: "Sunset Coral".to_owned(),
            center_color: Color::rgb(0xF5, 0x89, 0x62),
            edge_color: Color::rgb(0xB8, 0x37, 0x56),
            pattern_color: Color::rgb(0xFF, 0xCE, 0xB8),
            text_color: Color::WHITE,
        },
        ShareThemeBackdropOption {
            backdrop_id: 4,
            name: "Royal Violet".to_owned(),
            center_color: Color::rgb(0x88, 0x6A, 0xE6),
            edge_color: Color::rgb(0x42, 0x2B, 0x8E),
            pattern_color: Color::rgb(0xC8, 0xB8, 0xFF),
            text_color: Color::WHITE,
        },
        ShareThemeBackdropOption {
            backdrop_id: 5,
            name: "Amber Gold".to_owned(),
            center_color: Color::rgb(0xE8, 0xA8, 0x38),
            edge_color: Color::rgb(0x9E, 0x56, 0x14),
            pattern_color: Color::rgb(0xFF, 0xE2, 0x9A),
            text_color: Color::WHITE,
        },
    ]
}
