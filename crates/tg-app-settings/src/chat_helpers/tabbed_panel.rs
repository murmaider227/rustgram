//! Emoji / Stickers / GIFs panel: floating above the history compose bar (narrow windows) or
//! docked as a full-height third column (wide windows), with show/hide animation, rounded
//! shadow chrome in floating mode, and callback delivery.
//!
//! Port origin: `Telegram/SourceFiles/chat_helpers/tabbed_panel.h`,
//! `Telegram/SourceFiles/chat_helpers/tabbed_panel.cpp` and
//! `Telegram/SourceFiles/chat_helpers/tabbed_section.cpp` (docked `TabbedSection`).

use egui::{Area, Color32, CornerRadius, Frame, Id, Order, Pos2, Rect, Stroke, Ui, Vec2};
use tg_emoji::EmojiTextureCache;
use tg_ui::fonts::ensure_tdesktop_fonts;
use tg_ui::tg_style::st;

use crate::chat_helpers::backend::ChatHelpersBackend;
use crate::chat_helpers::emoji_list_widget::{DOCKED_PANEL_FLAG, theme_color};
use crate::chat_helpers::tabbed_selector::{ChatHelpersEvent, SelectorTab, TabbedSelector};
use tg_app_core::stand_in::theme::Palette;

/// Standard panel width (`st::emojiPanWidth`).
pub const TABBED_PANEL_WIDTH: f32 = st::emoji_pan_width.0 as f32;
/// Minimum panel content height (`st::emojiPanMinHeight`).
pub const TABBED_PANEL_MIN_HEIGHT: f32 = st::emoji_pan_min_height.0 as f32;
/// Maximum panel content height (`st::emojiPanMaxHeight`).
pub const TABBED_PANEL_MAX_HEIGHT: f32 = st::emoji_pan_max_height.0 as f32;
/// Default height ratio relative to available height (`st::emojiPanHeightRatio`).
pub const TABBED_PANEL_HEIGHT_RATIO: f32 = st::emoji_pan_height_ratio as f32;
/// Panel corner radius (`st::emojiPanRadius`).
pub const TABBED_PANEL_RADIUS: u8 = st::emoji_pan_radius.0 as u8;
/// Show/hide transition duration (`st::emojiPanShowDuration`).
pub const TABBED_PANEL_ANIM_DURATION_SECS: f64 = st::emoji_pan_show_duration as f64 / 1000.0;

/// Docked column width (`columnMinimalWidthThird`), used by `TabbedSection`.
pub const TABBED_SECTION_WIDTH: f32 = 292.0;
/// Widest docked column (`columnMaximalWidthThird`).
pub const TABBED_SECTION_MAX_WIDTH: f32 = 392.0;
/// Left (chat list) column minimum (`columnMinimalWidthLeft`).
const COLUMN_MIN_WIDTH_LEFT: f32 = 260.0;
/// Main (chat) column minimum (`columnMinimalWidthMain`).
const COLUMN_MIN_WIDTH_MAIN: f32 = 380.0;

/// Whether a window body of `body_width` px fits three columns, so the panel docks as a third
/// column instead of floating (`SessionController::minimalThreeColumnWidth`).
#[must_use]
pub fn panel_docks_at_width(body_width: f32, has_dialogs: bool) -> bool {
    let left = if has_dialogs {
        COLUMN_MIN_WIDTH_LEFT
    } else {
        0.0
    };
    body_width >= left + COLUMN_MIN_WIDTH_MAIN + TABBED_SECTION_WIDTH
}

/// Animation visibility state of [`TabbedPanel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelVisibility {
    /// Completely hidden; nothing is painted.
    Hidden,
    /// Animating open toward `progress = 1.0`.
    Showing,
    /// Fully visible at `progress = 1.0`.
    Visible,
    /// Animating closed toward `progress = 0.0`.
    Hiding,
}

/// Floating Emoji / Stickers / GIFs popup panel (`ChatHelpers::TabbedPanel`).
///
/// The history compose field creates a `TabbedPanel`, toggles it from the compose emoji button,
/// and calls [`TabbedPanel::show`] each frame with an event callback to insert emoji or send
/// stickers/GIFs.
#[derive(Debug)]
pub struct TabbedPanel {
    selector: TabbedSelector,
    visibility: PanelVisibility,
    anim_from: f32,
    anim_to: f32,
    anim_start: Option<f64>,
    drop_down: bool,
    height_ratio: f32,
    min_height: f32,
    max_height: f32,
}

impl Default for TabbedPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl TabbedPanel {
    /// Creates a new hidden [`TabbedPanel`] with tdesktop's default dimensions.
    #[must_use]
    pub fn new() -> Self {
        Self {
            selector: TabbedSelector::new(),
            visibility: PanelVisibility::Hidden,
            anim_from: 0.0,
            anim_to: 0.0,
            anim_start: None,
            drop_down: false,
            height_ratio: TABBED_PANEL_HEIGHT_RATIO,
            min_height: TABBED_PANEL_MIN_HEIGHT,
            max_height: TABBED_PANEL_MAX_HEIGHT,
        }
    }

    /// Returns a reference to the inner [`TabbedSelector`].
    #[must_use]
    pub const fn selector(&self) -> &TabbedSelector {
        &self.selector
    }

    /// Returns a mutable reference to the inner [`TabbedSelector`].
    pub fn selector_mut(&mut self) -> &mut TabbedSelector {
        &mut self.selector
    }

    /// Returns `true` once after the Stickers footer settings button was clicked (the host
    /// opens the installed sticker sets box).
    pub fn take_sticker_settings_requested(&mut self) -> bool {
        self.selector.take_sticker_settings_requested()
    }

    /// Configures drop-down placement (below anchor) vs drop-up placement (above compose bar).
    pub fn set_drop_down(&mut self, drop_down: bool) {
        self.drop_down = drop_down;
    }

    /// Configures desired height ratio and `[min_height, max_height]` bounds
    /// (`TabbedPanel::setDesiredHeightValues`).
    pub fn set_desired_height_values(&mut self, ratio: f32, min_height: f32, max_height: f32) {
        self.height_ratio = ratio.clamp(0.2, 1.0);
        self.min_height = min_height.max(180.0);
        self.max_height = max_height.max(self.min_height);
    }

    /// Returns the current [`PanelVisibility`] state.
    #[must_use]
    pub const fn visibility(&self) -> PanelVisibility {
        self.visibility
    }

    /// Returns `true` when the panel is open or animating open.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        matches!(
            self.visibility,
            PanelVisibility::Showing | PanelVisibility::Visible
        )
    }

    /// Returns `true` when the panel should still be painted (including during hide fade-out).
    #[must_use]
    pub const fn is_rendered(&self) -> bool {
        !matches!(self.visibility, PanelVisibility::Hidden)
    }

    /// Starts the animated show transition (`TabbedPanel::showAnimated`).
    pub fn show_animated(&mut self, now_secs: f64) {
        if self.visibility == PanelVisibility::Visible {
            return;
        }
        let cur = self.current_progress(now_secs);
        self.anim_from = cur;
        self.anim_to = 1.0;
        self.anim_start = Some(now_secs);
        self.visibility = PanelVisibility::Showing;
    }

    /// Opens the panel immediately without animation (useful for tests & static screenshots).
    pub fn show_immediate(&mut self) {
        self.visibility = PanelVisibility::Visible;
        self.anim_from = 1.0;
        self.anim_to = 1.0;
        self.anim_start = None;
    }

    /// Starts the animated hide transition (`TabbedPanel::hideAnimated`).
    pub fn hide_animated(&mut self, now_secs: f64) {
        if matches!(
            self.visibility,
            PanelVisibility::Hidden | PanelVisibility::Hiding
        ) {
            return;
        }
        let cur = self.current_progress(now_secs);
        self.anim_from = cur;
        self.anim_to = 0.0;
        self.anim_start = Some(now_secs);
        self.visibility = PanelVisibility::Hiding;
        self.selector.emoji_list_mut().close_skin_tone_popup();
    }

    /// Hides the panel immediately without animation (`TabbedPanel::hideFast`).
    pub fn hide_fast(&mut self) {
        self.visibility = PanelVisibility::Hidden;
        self.anim_from = 0.0;
        self.anim_to = 0.0;
        self.anim_start = None;
        self.selector.emoji_list_mut().close_skin_tone_popup();
    }

    /// Toggles between shown and hidden with animation (`TabbedPanel::toggleAnimated`).
    pub fn toggle_animated(&mut self, now_secs: f64) {
        if self.is_open() {
            self.hide_animated(now_secs);
        } else {
            self.show_animated(now_secs);
        }
    }

    /// Switches the active selector tab (e.g. `Emoji`, `Stickers`, `Gifs`).
    pub fn set_active_tab(&mut self, tab: SelectorTab) {
        self.selector.set_tab_immediate(tab);
    }

    /// Returns `true` while the panel show/hide transition animation is running.
    #[must_use]
    pub const fn is_animating(&self) -> bool {
        self.anim_start.is_some()
    }

    /// Computes the current eased animation progress in `[0.0, 1.0]`.
    #[must_use]
    pub fn current_progress(&self, now_secs: f64) -> f32 {
        match self.visibility {
            PanelVisibility::Hidden => 0.0,
            PanelVisibility::Visible => 1.0,
            PanelVisibility::Showing | PanelVisibility::Hiding => {
                let Some(start) = self.anim_start else {
                    return self.anim_to;
                };
                let raw =
                    ((now_secs - start) / TABBED_PANEL_ANIM_DURATION_SECS).clamp(0.0, 1.0) as f32;
                let eased = 1.0 - (1.0 - raw).powi(3);
                self.anim_from + (self.anim_to - self.anim_from) * eased
            }
        }
    }

    /// Computes the panel's target screen rectangle given the anchor button/bar rect and viewport.
    #[must_use]
    pub fn compute_panel_rect(&self, anchor_rect: Rect, viewport: Rect) -> Rect {
        let width = TABBED_PANEL_WIDTH.min((viewport.width() - 16.0).max(260.0));
        let available_h = if self.drop_down {
            (viewport.bottom() - anchor_rect.bottom() - 16.0).max(0.0)
        } else {
            (anchor_rect.top() - viewport.top() - 16.0).max(0.0)
        };
        let desired_h = (available_h * self.height_ratio)
            .clamp(self.min_height, self.max_height)
            .min(available_h.max(self.min_height));

        let right = anchor_rect
            .right()
            .min(viewport.right() - 8.0)
            .max(viewport.left() + width + 8.0);
        let left = (right - width).max(viewport.left() + 8.0);
        let top = if self.drop_down {
            anchor_rect.bottom() + 6.0
        } else {
            (anchor_rect.top() - desired_h - 10.0).max(viewport.top() + 8.0)
        };
        Rect::from_min_size(Pos2::new(left, top), Vec2::new(width, desired_h))
    }

    /// Renders the panel docked as a full-height column filling `rect` (`TabbedSection`):
    /// no shadow, no rounded corners, a `shadowFg` line on the left edge, and no
    /// click-outside dismissal. The host reserves the column (Package A's layout slot).
    ///
    /// Returns `rect` while the panel is shown, `None` when hidden.
    pub fn show_docked<F>(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        backend: &mut dyn ChatHelpersBackend,
        emoji_cache: &mut EmojiTextureCache,
        palette: &Palette,
        mut on_event: F,
    ) -> Option<Rect>
    where
        F: FnMut(ChatHelpersEvent),
    {
        if !self.is_rendered() {
            return None;
        }
        let ctx = ui.ctx().clone();
        ensure_tdesktop_fonts(&ctx);
        if self.visibility != PanelVisibility::Visible {
            // A docked section has no show animation (`TabbedSection` is a plain column).
            match self.visibility {
                PanelVisibility::Hiding => {
                    self.hide_fast();
                    return None;
                }
                _ => self.show_immediate(),
            }
        }
        let now = ctx.input(|i| i.time);
        for ev in self.selector.handle_keyboard(&ctx, backend) {
            if ev == ChatHelpersEvent::CloseRequested {
                self.hide_animated(now);
            } else {
                on_event(ev);
            }
        }

        ctx.data_mut(|d| d.insert_temp(Id::unique(DOCKED_PANEL_FLAG), true));
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, palette.window_bg);
        let mut close = false;
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            ui.set_clip_rect(rect.intersect(ui.clip_rect()));
            ui.set_min_size(rect.size());
            ui.set_max_size(rect.size());
            for ev in self.selector.show(ui, backend, emoji_cache, palette) {
                if ev == ChatHelpersEvent::CloseRequested {
                    close = true;
                } else {
                    on_event(ev);
                }
            }
        });
        ui.painter().line_segment(
            [rect.left_top(), rect.left_bottom()],
            Stroke::new(1.0, theme_color(&ctx, "shadowFg", palette.divider)),
        );
        ctx.data_mut(|d| d.insert_temp(Id::unique(DOCKED_PANEL_FLAG), false));
        if close {
            self.hide_fast();
        }
        Some(rect)
    }

    /// Renders the floating [`TabbedPanel`] above (or below) `anchor_rect` and invokes `on_event`
    /// for each chosen emoji, sticker, GIF, or premium lock event.
    ///
    /// Returns the rendered panel rectangle when visible.
    pub fn show<F>(
        &mut self,
        ctx: &egui::Context,
        anchor_rect: Rect,
        backend: &mut dyn ChatHelpersBackend,
        emoji_cache: &mut EmojiTextureCache,
        palette: &Palette,
        mut on_event: F,
    ) -> Option<Rect>
    where
        F: FnMut(ChatHelpersEvent),
    {
        if !self.is_rendered() {
            return None;
        }
        ensure_tdesktop_fonts(ctx);
        ctx.data_mut(|d| d.insert_temp(Id::unique(DOCKED_PANEL_FLAG), false));

        let now = ctx.input(|i| i.time);
        let progress = self.current_progress(now);

        // Advance animation completion state.
        if let Some(start) = self.anim_start {
            if now - start >= TABBED_PANEL_ANIM_DURATION_SECS {
                self.anim_start = None;
                match self.visibility {
                    PanelVisibility::Showing => self.visibility = PanelVisibility::Visible,
                    PanelVisibility::Hiding => {
                        self.visibility = PanelVisibility::Hidden;
                        return None;
                    }
                    _ => {}
                }
            } else {
                ctx.request_repaint();
            }
        }

        // Process keyboard shortcuts only when panel is open (not hiding).
        if self.is_open() {
            for ev in self.selector.handle_keyboard(ctx, backend) {
                if ev == ChatHelpersEvent::CloseRequested {
                    self.hide_animated(now);
                } else {
                    on_event(ev);
                }
            }
        }

        let viewport = ctx.content_rect();
        let target_rect = self.compute_panel_rect(anchor_rect, viewport);

        // Subtle rise/scale animation from bottom-right (or top-right in drop-down mode).
        let y_shift = if self.drop_down {
            -(1.0 - progress) * 10.0
        } else {
            (1.0 - progress) * 10.0
        };
        let animated_rect = target_rect.translate(Vec2::new(0.0, y_shift));
        let alpha = progress.clamp(0.0, 1.0);

        Area::new(Id::unique("tg_tabbed_panel_area"))
            .order(Order::Foreground)
            .constrain(false)
            .fixed_pos(animated_rect.min)
            .show(ctx, |ui| {
                ui.set_min_size(animated_rect.size());
                ui.set_max_size(animated_rect.size());
                ui.multiply_opacity(alpha);

                // Layered soft drop shadow matching `st::emojiPanAnimation.shadow`.
                let shadow_alpha = (32.0 * alpha).round() as u8;
                ui.painter().rect_filled(
                    animated_rect.expand(4.0).translate(Vec2::new(0.0, 3.0)),
                    CornerRadius::same(TABBED_PANEL_RADIUS + 3),
                    Color32::from_rgba_unmultiplied(0, 0, 0, shadow_alpha / 2),
                );
                ui.painter().rect_filled(
                    animated_rect.expand(1.5).translate(Vec2::new(0.0, 1.5)),
                    CornerRadius::same(TABBED_PANEL_RADIUS + 1),
                    Color32::from_rgba_unmultiplied(0, 0, 0, shadow_alpha),
                );

                Frame::NONE
                    .fill(palette.window_bg)
                    .stroke(Stroke::new(1.0, palette.divider))
                    .corner_radius(CornerRadius::same(TABBED_PANEL_RADIUS))
                    .show(ui, |ui| {
                        ui.scope_builder(egui::UiBuilder::new().max_rect(animated_rect), |ui| {
                            ui.set_min_size(animated_rect.size());
                            ui.set_max_size(animated_rect.size());
                            let events = self.selector.show(ui, backend, emoji_cache, palette);
                            for ev in events {
                                if ev == ChatHelpersEvent::CloseRequested {
                                    self.hide_animated(now);
                                } else {
                                    on_event(ev);
                                }
                            }
                        });
                    });
            });

        // Clicking outside both the panel and its anchor button dismisses the panel (unless skin popup open).
        if self.is_open()
            && !self.selector.prevent_auto_hide()
            && ctx.input(|i| i.pointer.any_click())
            && let Some(pos) = ctx.input(|i| i.pointer.interact_pos())
            && !animated_rect.contains(pos)
            && !anchor_rect.contains(pos)
        {
            self.hide_animated(now);
        }

        Some(animated_rect)
    }
}
