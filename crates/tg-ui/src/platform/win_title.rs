//! Custom Windows frameless window title bar with minimize, maximize/restore,
//! and close buttons.
//!
//! Port of `Telegram/lib_ui/ui/platform/win/ui_window_title_win.h`,
//! `Telegram/lib_ui/ui/platform/win/ui_window_title_win.cpp`, and
//! `Telegram/lib_ui/ui/platform/win/ui_window_win.cpp`.

use egui::{Align2, CornerRadius, CursorIcon, Pos2, Rect, Sense, Ui, Vec2, ViewportCommand};

use crate::anim::{Transition, color_lerp};
use crate::painter::HoverAnimState;
use crate::style::{WindowTitleStyle, constants};
use crate::widgets::buttons::{IconKind, paint_icon};

/// Action emitted by [`WindowTitleBar::show`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleBarAction {
    None,
    Minimize,
    ToggleMaximize,
    Close,
    DragStart,
}

/// Custom Windows title bar mirroring `Ui::Platform::TitleWidget`.
///
/// Supports both real frameless window control (`issue_viewport_commands = true`)
/// and embedded preview rendering (`issue_viewport_commands = false`).
pub struct WindowTitleBar<'a> {
    title: &'a str,
    style: WindowTitleStyle,
    active: bool,
    maximized: bool,
    issue_viewport_commands: bool,
    id_salt: &'a str,
}

impl<'a> WindowTitleBar<'a> {
    /// Create a new custom Windows title bar.
    pub fn new(title: &'a str, style: WindowTitleStyle) -> Self {
        Self {
            title,
            style,
            active: true,
            maximized: false,
            issue_viewport_commands: true,
            id_salt: "tg_win_title_bar",
        }
    }

    /// Set whether the window is focused (`bgActive`/`fgActive` vs `bg`/`fg`).
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    /// Set whether the window is currently maximized (switches Maximize -> Restore glyph).
    pub fn maximized(mut self, maximized: bool) -> Self {
        self.maximized = maximized;
        self
    }

    /// Enable or disable issuing `ViewportCommand` directly to the OS window.
    pub fn issue_viewport_commands(mut self, issue: bool) -> Self {
        self.issue_viewport_commands = issue;
        self
    }

    /// Set a custom ID salt when showing multiple preview title bars on one screen.
    pub fn id_salt(mut self, salt: &'a str) -> Self {
        self.id_salt = salt;
        self
    }

    /// Render the title bar across the full available width of `ui`.
    pub fn show(self, ui: &mut Ui) -> TitleBarAction {
        let width = ui.available_width().max(120.0);
        let height = self.style.height;
        let id = ui.make_persistent_id(self.id_salt);
        let (bar_rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());

        let bg = if self.active {
            self.style.bg_active
        } else {
            self.style.bg
        };
        let fg = if self.active {
            self.style.fg_active
        } else {
            self.style.fg
        };

        if ui.is_rect_visible(bar_rect) {
            ui.painter().rect_filled(bar_rect, CornerRadius::ZERO, bg);
        }

        let btn_w = self.style.button_width;
        let controls_w = btn_w * 3.0;
        let caption_rect = Rect::from_min_max(
            bar_rect.min,
            Pos2::new(
                (bar_rect.max.x - controls_w).max(bar_rect.min.x),
                bar_rect.max.y,
            ),
        );

        let mut action = TitleBarAction::None;

        // Caption drag / double-click maximize region.
        let cap_resp = ui.interact(caption_rect, id.with("caption"), Sense::click_and_drag());
        if cap_resp.double_clicked() {
            action = TitleBarAction::ToggleMaximize;
            if self.issue_viewport_commands {
                ui.ctx()
                    .send_viewport_cmd(ViewportCommand::Maximized(!self.maximized));
            }
        } else if cap_resp.drag_started() {
            action = TitleBarAction::DragStart;
            if self.issue_viewport_commands {
                ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
            }
        }

        if ui.is_rect_visible(caption_rect) && !self.title.is_empty() {
            ui.painter().text(
                Pos2::new(caption_rect.min.x + 12.0, caption_rect.center().y),
                Align2::LEFT_CENTER,
                self.title,
                self.style.font.font_id(),
                fg,
            );
        }

        // Right-aligned Windows control buttons: Minimize, Maximize/Restore, Close.
        let min_rect = Rect::from_min_size(
            Pos2::new(bar_rect.max.x - btn_w * 3.0, bar_rect.min.y),
            Vec2::new(btn_w, height),
        );
        let max_rect = Rect::from_min_size(
            Pos2::new(bar_rect.max.x - btn_w * 2.0, bar_rect.min.y),
            Vec2::new(btn_w, height),
        );
        let close_rect = Rect::from_min_size(
            Pos2::new(bar_rect.max.x - btn_w, bar_rect.min.y),
            Vec2::new(btn_w, height),
        );

        if self.draw_control_button(ui, id.with("min"), min_rect, IconKind::Minimize, false) {
            action = TitleBarAction::Minimize;
            if self.issue_viewport_commands {
                ui.ctx().send_viewport_cmd(ViewportCommand::Minimized(true));
            }
        }

        let max_icon = if self.maximized {
            IconKind::Restore
        } else {
            IconKind::Maximize
        };
        if self.draw_control_button(ui, id.with("max"), max_rect, max_icon, false) {
            action = TitleBarAction::ToggleMaximize;
            if self.issue_viewport_commands {
                ui.ctx()
                    .send_viewport_cmd(ViewportCommand::Maximized(!self.maximized));
            }
        }

        if self.draw_control_button(ui, id.with("close"), close_rect, IconKind::Close, true) {
            action = TitleBarAction::Close;
            if self.issue_viewport_commands {
                ui.ctx().send_viewport_cmd(ViewportCommand::Close);
            }
        }

        // 1px bottom shadow hairline (`titleShadow`).
        if self.style.shadow && ui.is_rect_visible(bar_rect) {
            ui.painter().rect_filled(
                Rect::from_min_max(
                    Pos2::new(bar_rect.min.x, bar_rect.max.y - constants::LINE_WIDTH),
                    bar_rect.max,
                ),
                CornerRadius::ZERO,
                self.style.shadow_fg,
            );
        }

        action
    }

    fn draw_control_button(
        &self,
        ui: &mut Ui,
        id: egui::Id,
        rect: Rect,
        icon: IconKind,
        is_close: bool,
    ) -> bool {
        let resp = ui.interact(rect, id, Sense::click());
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::Default);
        }
        let over = HoverAnimState::update(
            ui.ctx(),
            id,
            resp.hovered() || resp.is_pointer_button_down_on(),
            constants::UNIVERSAL_DURATION_MS,
            Transition::SineInOut,
        );
        if ui.is_rect_visible(rect) {
            let (bg_base, bg_over, fg_base, fg_over) = if is_close {
                (
                    self.style.button_bg,
                    self.style.close_bg_over,
                    self.style.button_fg,
                    self.style.close_fg_over,
                )
            } else {
                (
                    self.style.button_bg,
                    self.style.button_bg_over,
                    self.style.button_fg,
                    self.style.button_fg_over,
                )
            };
            let bg = color_lerp(bg_base, bg_over, over);
            let fg = color_lerp(fg_base, fg_over, over);
            ui.painter().rect_filled(rect, CornerRadius::ZERO, bg);
            let icon_rect = Rect::from_center_size(rect.center(), Vec2::splat(13.0));
            paint_icon(ui.painter(), icon_rect, icon, fg);
        }
        resp.clicked()
    }
}
