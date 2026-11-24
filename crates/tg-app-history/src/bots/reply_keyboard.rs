//! Custom reply keyboard widget replacing/expanding below the compose field area, plus the
//! compose-bar reply-keyboard toggle button.
//!
//! Port origin: `Telegram/SourceFiles/chat_helpers/bot_keyboard.h`,
//! `Telegram/SourceFiles/chat_helpers/bot_keyboard.cpp`, and
//! `Telegram/SourceFiles/history/history_widget.cpp` (bot keyboard toggle & visibility rules).

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Pos2, Rect, Response, ScrollArea, Sense,
    Stroke, StrokeKind, Ui, Vec2,
};
use tg_bots::callback::{BotButtonAction, ButtonActivationContext, activate_button};
use tg_bots::keyboard_layout::{
    BOT_KB_SCROLL_DELTA_BOTTOM, BOT_KB_SCROLL_DELTA_TOP, BotKeyboardMetrics, ButtonRounding,
    ButtonsLayout,
};
use tg_bots::markup::{ButtonType, MarkupButton, ReplyKeyboardState, ReplyMarkupData};
use tg_bots::stand_in::{FullMsgId, MsgId, UserId};

use crate::bots::ui_helpers::{
    BotButtonColors, paint_reply_button_leading_icon, to_corner_radius, with_alpha,
};
use tg_app_core::stand_in::theme::Palette;

/// Default maximum outer height of the reply keyboard panel (`220px`, matches tdesktop's
/// `st::botKbMaxHeight`).
pub const REPLY_KEYBOARD_MAX_HEIGHT: i32 = 220;

/// Event emitted when the user activates a button on [`ReplyKeyboardWidget`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyKeyboardActivation {
    /// Owning message id.
    pub msg_id: FullMsgId,
    /// Row index.
    pub row: usize,
    /// Column index.
    pub col: usize,
    /// Activated [`MarkupButton`].
    pub button: MarkupButton,
    /// Resolved [`BotButtonAction`].
    pub action: BotButtonAction,
    /// `true` if activating this button automatically dismissed a `single_use` keyboard.
    pub dismissed_single_use: bool,
}

/// Stateful reply keyboard panel (`BotKeyboard` port).
#[derive(Debug, Clone, Default)]
pub struct ReplyKeyboardWidget {
    /// Derived state from `tg_bots::markup::ReplyKeyboardState`.
    pub state: ReplyKeyboardState,
    /// Current reply markup data (non-inline).
    markup: Option<ReplyMarkupData>,
    /// Whether the reply keyboard drawer is currently shown (toggled from compose bar).
    pub visible: bool,
    /// Maximum outer height in pixels when `maximize_size` (`!resize`) is active.
    pub max_outer_height: i32,
}

impl ReplyKeyboardWidget {
    /// Creates an empty [`ReplyKeyboardWidget`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: ReplyKeyboardState::default(),
            markup: None,
            visible: false,
            max_outer_height: REPLY_KEYBOARD_MAX_HEIGHT,
        }
    }

    /// Updates the reply keyboard from a message's [`ReplyMarkupData`].
    ///
    /// Honors `RemoveKeyboard`, `resize`, `single_use`, `persistent`, `selective`, `force_reply`,
    /// and `placeholder` via [`ReplyKeyboardState::update_markup`]. Returns `true` if changed.
    pub fn update_markup(
        &mut self,
        msg_id: Option<FullMsgId>,
        markup: Option<&ReplyMarkupData>,
        force: bool,
    ) -> bool {
        let changed = self.state.update_markup(msg_id, markup, force);
        if !changed {
            return false;
        }
        if self.state.has_visible_rows {
            self.markup = markup.cloned();
            // Auto-show when persistent or freshly received.
            self.visible = true;
        } else {
            self.markup = None;
            self.visible = false;
        }
        true
    }

    /// Clears the active reply keyboard.
    pub fn clear(&mut self) {
        self.state.clear();
        self.markup = None;
        self.visible = false;
    }

    /// Returns `true` if a non-empty reply keyboard is attached to the current chat.
    #[must_use]
    pub fn has_markup(&self) -> bool {
        self.state.has_visible_rows && self.markup.is_some()
    }

    /// Returns `true` if the reply keyboard should currently be painted.
    #[must_use]
    pub fn is_shown(&self) -> bool {
        self.has_markup() && self.visible
    }

    /// Toggles visibility (when `has_markup()` is true).
    ///
    /// Note: when `persistent` is false, toggling hides the drawer; pressing the compose bar
    /// keyboard icon brings it back.
    pub fn toggle_visible(&mut self) {
        if self.has_markup() {
            self.visible = !self.visible;
        }
    }

    /// Returns the compose input placeholder text supplied by the active reply keyboard, if any.
    #[must_use]
    pub fn placeholder(&self) -> Option<&str> {
        if self.state.placeholder.is_empty() {
            None
        } else {
            Some(&self.state.placeholder)
        }
    }

    /// Returns a summary of active reply keyboard flags for debugging/header chips.
    #[must_use]
    pub fn active_flag_labels(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if !self.state.maximize_size {
            out.push("resize");
        } else {
            out.push("maximized");
        }
        if self.state.single_use {
            out.push("single_use");
        }
        if self.state.persistent {
            out.push("persistent");
        }
        if self.state.selective {
            out.push("selective");
        }
        if self.state.force_reply {
            out.push("force_reply");
        }
        out
    }

    /// Computes the exact pixel height required to paint the reply keyboard panel at `width`
    /// (`0.0` when hidden or empty).
    #[must_use]
    pub fn desired_height(&self, ui: &mut Ui, width: f32) -> f32 {
        if !self.is_shown() {
            return 0.0;
        }
        let Some(ref markup) = self.markup else {
            return 0.0;
        };
        let outer_w = width.max(180.0);
        let font = FontId::proportional(13.5);
        let mut layout =
            ButtonsLayout::from_markup(markup, BotKeyboardMetrics::REPLY_NORMAL, |text| {
                let w = ui.fonts_mut(|f| {
                    f.layout_no_wrap(text.to_owned(), font.clone(), Color32::WHITE)
                        .size()
                        .x
                });
                (w.ceil() as i32) + 18
            });
        let total_height = layout.resize_reply_keyboard(
            outer_w.round() as i32,
            self.max_outer_height,
            self.state.maximize_size,
        );
        (total_height as f32).min(self.max_outer_height as f32) + 1.0
    }

    /// Renders the reply keyboard panel below the compose bar.
    ///
    /// Uses `tg_bots::keyboard_layout::ButtonsLayout::resize_reply_keyboard` to automatically
    /// switch between `REPLY_NORMAL` (38px height, 10px gap) and `REPLY_TINY` (25px height, 4px
    /// gap) when a row with >3 buttons overflows the available width.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        bot_id: Option<UserId>,
        topic_root_id: Option<MsgId>,
        palette: &Palette,
    ) -> Option<ReplyKeyboardActivation> {
        if !self.is_shown() {
            return None;
        }
        let markup = self.markup.clone()?;
        let msg_id = self.state.for_msg_id?;

        let outer_w = ui.available_width().max(180.0);
        let font = FontId::proportional(13.5);
        let mut layout =
            ButtonsLayout::from_markup(&markup, BotKeyboardMetrics::REPLY_NORMAL, |text| {
                let w = ui.fonts_mut(|f| {
                    f.layout_no_wrap(text.to_owned(), font.clone(), Color32::WHITE)
                        .size()
                        .x
                });
                // Reserve a small allowance when a leading icon is drawn.
                (w.ceil() as i32) + 18
            });

        let total_height = layout.resize_reply_keyboard(
            outer_w.round() as i32,
            self.max_outer_height,
            self.state.maximize_size,
        );
        let panel_h = (total_height as f32).min(self.max_outer_height as f32);
        let chosen_metrics = layout.metrics();
        let is_tiny = chosen_metrics == BotKeyboardMetrics::REPLY_TINY;
        let label_font = FontId::proportional(if is_tiny { 12.0 } else { 13.5 });

        let colors = BotButtonColors::from_palette(palette);
        let mut activation = None;

        // Top hairline separating compose bar and reply keyboard drawer.
        let (sep_rect, _) = ui.allocate_exact_size(Vec2::new(outer_w, 1.0), Sense::hover());
        ui.painter()
            .rect_filled(sep_rect, CornerRadius::ZERO, palette.divider);

        let bg_rect = Rect::from_min_size(ui.cursor().min, Vec2::new(outer_w, panel_h));
        ui.painter()
            .rect_filled(bg_rect, CornerRadius::ZERO, palette.window_bg);

        ScrollArea::vertical()
            .id_salt(("tg_bot_reply_kb_scroll", msg_id.peer.0, msg_id.msg.0))
            .max_height(panel_h)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let content_h = total_height.max(panel_h.round() as i32) as f32;
                let (canvas_rect, _) =
                    ui.allocate_exact_size(Vec2::new(outer_w, content_h), Sense::hover());
                let offset_x = chosen_metrics.button_skip as f32;
                let offset_y = BOT_KB_SCROLL_DELTA_TOP as f32;

                for laid_row in layout.rows() {
                    for laid in laid_row {
                        let Some(button) = markup.button(laid.row, laid.column) else {
                            continue;
                        };
                        let btn_rect = Rect::from_min_size(
                            Pos2::new(
                                canvas_rect.left() + offset_x + laid.rect.x as f32,
                                canvas_rect.top() + offset_y + laid.rect.y as f32,
                            ),
                            Vec2::new(laid.rect.width as f32, laid.rect.height as f32),
                        );

                        let id = Id::unique((
                            "tg_reply_kb_btn",
                            msg_id.peer.0,
                            msg_id.msg.0,
                            laid.row,
                            laid.column,
                        ));
                        let disabled = button.button_type() == ButtonType::Disabled;
                        let resp = ui.interact(
                            btn_rect,
                            id,
                            if disabled {
                                Sense::hover()
                            } else {
                                Sense::click()
                            },
                        );
                        resp.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::Role::Button,
                                !disabled,
                                button.text.clone(),
                            )
                        });
                        let hovered = !disabled && resp.hovered();
                        let pressed = !disabled && resp.is_pointer_button_down_on();
                        if hovered {
                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        }

                        let rounding = layout.button_rounding(
                            laid.row,
                            laid.column,
                            ButtonRounding::ALL_LARGE,
                            false,
                        );
                        let corner_radius = to_corner_radius(rounding);
                        let (bg, fg) = colors.resolve(button.visual.color, false, hovered, pressed);

                        let painter = ui.painter();
                        painter.rect_filled(btn_rect, corner_radius, bg);

                        let has_icon = matches!(
                            button.button_type(),
                            ButtonType::RequestPhone
                                | ButtonType::RequestLocation
                                | ButtonType::RequestPoll
                                | ButtonType::RequestPeer
                                | ButtonType::CreateBot
                                | ButtonType::SimpleWebView
                                | ButtonType::WebView
                        );

                        let galley = ui.fonts_mut(|f| {
                            f.layout_no_wrap(button.text.clone(), label_font.clone(), fg)
                        });
                        let icon_gap = if has_icon { 16.0 } else { 0.0 };
                        let total_content_w = galley.size().x + icon_gap;
                        let content_left = (btn_rect.center().x - total_content_w / 2.0)
                            .max(btn_rect.left() + 8.0);

                        if has_icon {
                            paint_reply_button_leading_icon(
                                painter,
                                Pos2::new(content_left + 5.0, btn_rect.center().y),
                                button.button_type(),
                                with_alpha(fg, 0.85),
                            );
                        }

                        painter.text(
                            Pos2::new(content_left + icon_gap, btn_rect.center().y),
                            Align2::LEFT_CENTER,
                            &button.text,
                            label_font.clone(),
                            fg,
                        );

                        if !disabled && resp.clicked() {
                            let ctx = ButtonActivationContext {
                                item_id: msg_id,
                                topic_root_id,
                                bot_id,
                                bot_verified: false,
                                bot_inline_return_to: None,
                                is_regular: true,
                                ephemeral_id: None,
                                request_in_flight: false,
                            };
                            let action = activate_button(button, &ctx);
                            if !matches!(action, BotButtonAction::NoOp) {
                                activation = Some((laid.row, laid.column, button.clone(), action));
                            }
                        }
                    }
                }
            });

        // Bottom padding is accounted for in `total_height` via `BOT_KB_SCROLL_DELTA_BOTTOM`.
        let _ = BOT_KB_SCROLL_DELTA_BOTTOM;

        if let Some((row, col, button, action)) = activation {
            let dismissed_single_use = self.state.hide_if_single_use(msg_id);
            if dismissed_single_use {
                self.markup = None;
                self.visible = false;
            }
            return Some(ReplyKeyboardActivation {
                msg_id,
                row,
                col,
                button,
                action,
                dismissed_single_use,
            });
        }

        None
    }
}

/// Paints the reply-keyboard toggle icon button inside the compose bar (`history_widget.cpp`).
///
/// Shows a 4-key grid icon when the keyboard is collapsed, and highlights in accent blue
/// (with a chevron-down badge) while the reply keyboard is expanded.
pub fn paint_reply_keyboard_toggle_button(
    ui: &mut Ui,
    palette: &Palette,
    keyboard_visible: bool,
    has_keyboard: bool,
) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(34.0), Sense::click());
    if !has_keyboard {
        return resp;
    }
    resp.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::Role::Button,
            has_keyboard,
            keyboard_visible,
            if keyboard_visible {
                "Hide bot keyboard"
            } else {
                "Show bot keyboard"
            },
        )
    });
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .circle_filled(rect.center(), 16.0, palette.window_bg_over);
    }

    let color = if keyboard_visible {
        palette.window_bg_active
    } else if resp.hovered() {
        palette.window_fg
    } else {
        palette.window_sub_text_fg
    };

    let c = rect.center();
    let frame = Rect::from_center_size(c, Vec2::new(18.0, 14.0));
    ui.painter().rect_stroke(
        frame,
        CornerRadius::same(3),
        Stroke::new(1.5, color),
        StrokeKind::Inside,
    );

    if keyboard_visible {
        // Active state: filled top keys + chevron indicator
        let dot_y = c.y - 2.0;
        for dx in [-4.5_f32, 0.0, 4.5] {
            ui.painter().rect_filled(
                Rect::from_center_size(Pos2::new(c.x + dx, dot_y), Vec2::splat(2.5)),
                CornerRadius::same(1),
                color,
            );
        }
        ui.painter().line_segment(
            [
                Pos2::new(c.x - 4.0, c.y + 3.0),
                Pos2::new(c.x + 4.0, c.y + 3.0),
            ],
            Stroke::new(1.6, color),
        );
    } else {
        // 2x3 key grid dots
        for dy in [-2.2_f32, 2.2] {
            for dx in [-4.5_f32, 0.0, 4.5] {
                ui.painter().rect_filled(
                    Rect::from_center_size(Pos2::new(c.x + dx, c.y + dy), Vec2::splat(2.2)),
                    CornerRadius::same(1),
                    color,
                );
            }
        }
    }

    resp.on_hover_text(if keyboard_visible {
        "Hide bot keyboard"
    } else {
        "Show bot keyboard"
    })
}
