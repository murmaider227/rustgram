//! Hover chat preview popup, `Ctrl+Tab` chat-switch overlay, separate saved chat windows
//! (via `egui` viewports), and custom frameless Win32 title bar helper.
//!
//! Port of:
//! - `Telegram/SourceFiles/window/window_chat_preview.{h,cpp}`
//! - `Telegram/SourceFiles/window/window_chat_switch_process.{h,cpp}`
//! - `Telegram/SourceFiles/window/window_saved_windows.{h,cpp}`
//! - `Telegram/SourceFiles/platform/win/main_window_win.{h,cpp}` (frameless title bar / Aero snap)

use std::collections::VecDeque;
use std::path::Path;

use egui::{
    Align2, Area, Color32, CornerRadius, CursorIcon, Id, Key, Order, Pos2, Rect, ResizeDirection,
    ScrollArea, Sense, Stroke, StrokeKind, Ui, Vec2, ViewportBuilder, ViewportCommand, ViewportId,
};
use serde::{Deserialize, Serialize};
use tg_platform::frameless::{
    DEFAULT_RESIZE_BORDER_PX, FramelessHitZone, ResizeEdge, apply_frameless_aero_snap,
    apply_window_dark_mode, hit_test_frameless,
};
use tg_ui::fonts::font_id_from_style;
use tg_ui::tg_style::st;

use tg_app_core::stand_in::theme::Palette;
use tg_app_core::stand_in::types::PeerId;

/// Delay in seconds before a hovered dialog row triggers [`ChatPreviewManager`].
pub const CHAT_PREVIEW_HOVER_DELAY_SECS: f64 = 0.45;

/// Default width of the floating hover chat preview card.
pub const CHAT_PREVIEW_WIDTH: f32 = 340.0;

/// Default height of the floating hover chat preview card.
pub const CHAT_PREVIEW_HEIGHT: f32 = 320.0;

/// Maximum separate saved windows tracked (`kMaxSavedWindows = 64`).
pub const MAX_SAVED_WINDOWS: usize = 64;

/// Maximum closed separate windows remembered for `Ctrl+Shift+T` (`kMaxClosedWindows = 16`).
pub const MAX_CLOSED_WINDOWS: usize = 16;

/// Number of tile columns in the `Ctrl+Tab` chat-switch overlay (`st::chatSwitchPerRow`).
pub const CHAT_SWITCH_COLUMNS: usize = 5;

/// Tile dimensions in the `Ctrl+Tab` chat-switch overlay (`st::chatSwitchSize`).
pub const CHAT_SWITCH_TILE_SIZE: Vec2 = Vec2::new(88.0, 96.0);

/// Filename used to persist separate saved chat windows inside `tdata/`.
pub const SAVED_WINDOWS_FILENAME: &str = "saved-windows.json";

/// Compact message bubble displayed inside the hover chat preview card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatPreviewMessage {
    /// Sender display name (empty for outgoing or 1:1 incoming).
    pub sender: String,
    /// Preview message body text.
    pub text: String,
    /// Formatted short timestamp (e.g. `"14:32"`).
    pub time_text: String,
    /// Whether the message was sent by the current user.
    pub outgoing: bool,
}

/// Snapshot of a chat rendered inside [`ChatPreviewOverlay`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatPreviewData {
    /// Target peer ID.
    pub peer_id: PeerId,
    /// Chat display title.
    pub title: String,
    /// Status / member count subtitle.
    pub subtitle: String,
    /// Avatar initials fallback.
    pub initials: String,
    /// Palette avatar color index (`0..7`).
    pub color_index: u8,
    /// Unread message count badge.
    pub unread_count: u32,
    /// Whether notifications are muted.
    pub muted: bool,
    /// Recent messages ordered oldest to newest.
    pub messages: Vec<ChatPreviewMessage>,
}

/// Action emitted by [`ChatPreviewManager::show`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatPreviewAction {
    /// Open the previewed chat in the main window.
    OpenChat(PeerId),
    /// Pop the previewed chat out into a separate window viewport.
    OpenSeparateWindow(PeerId),
    /// Mark the previewed chat as read.
    MarkRead(PeerId),
    /// Toggle mute state for the previewed chat.
    ToggleMute(PeerId),
    /// Dismissed the preview card.
    Closed,
}

/// Stateful controller for hover-triggered and shortcut-triggered chat previews
/// (`Window::ChatPreviewManager`).
#[derive(Debug, Default)]
pub struct ChatPreviewManager {
    scheduled: Option<(PeerId, Pos2, f64)>,
    active: Option<(ChatPreviewData, Pos2)>,
}

impl ChatPreviewManager {
    /// Creates an idle [`ChatPreviewManager`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            scheduled: None,
            active: None,
        }
    }

    /// Returns `true` while a preview popup is currently open.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.active.is_some()
    }

    /// Returns the currently previewed peer ID, if open.
    #[must_use]
    pub fn active_peer(&self) -> Option<PeerId> {
        self.active.as_ref().map(|(d, _)| d.peer_id)
    }

    /// Schedules a hover preview for `peer_id` anchored near `anchor` after
    /// [`CHAT_PREVIEW_HOVER_DELAY_SECS`].
    pub fn schedule(&mut self, peer_id: PeerId, anchor: Pos2, now_secs: f64) {
        if let Some((scheduled_peer, _, _)) = self.scheduled
            && scheduled_peer == peer_id
        {
            return;
        }
        self.scheduled = Some((peer_id, anchor, now_secs + CHAT_PREVIEW_HOVER_DELAY_SECS));
    }

    /// Cancels any pending hover timer without closing an already open preview.
    pub fn cancel_scheduled(&mut self) {
        self.scheduled = None;
    }

    /// Checks whether a scheduled hover delay elapsed, returning `(peer_id, anchor)` to populate.
    pub fn poll_scheduled(&mut self, now_secs: f64) -> Option<(PeerId, Pos2)> {
        let (peer_id, anchor, due) = self.scheduled?;
        if now_secs >= due {
            self.scheduled = None;
            return Some((peer_id, anchor));
        }
        None
    }

    /// Opens the chat preview popup immediately with `data` anchored at `anchor`.
    pub fn show_data(&mut self, data: ChatPreviewData, anchor: Pos2) {
        self.scheduled = None;
        self.active = Some((data, anchor));
    }

    /// Closes the preview popup and cancels any pending hover timer.
    pub fn close(&mut self) {
        self.scheduled = None;
        self.active = None;
    }

    /// Renders the floating chat preview card inside `viewport_rect`.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        viewport_rect: Rect,
        palette: &Palette,
    ) -> Option<ChatPreviewAction> {
        let (data, anchor) = self.active.clone()?;

        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.close();
            return Some(ChatPreviewAction::Closed);
        }

        let card_size = Vec2::new(
            CHAT_PREVIEW_WIDTH.min((viewport_rect.width() - 24.0).max(200.0)),
            CHAT_PREVIEW_HEIGHT.min((viewport_rect.height() - 24.0).max(180.0)),
        );
        let clamped_x = anchor.x.clamp(
            viewport_rect.left() + 12.0,
            (viewport_rect.right() - card_size.x - 12.0).max(viewport_rect.left() + 12.0),
        );
        let clamped_y = anchor.y.clamp(
            viewport_rect.top() + 12.0,
            (viewport_rect.bottom() - card_size.y - 12.0).max(viewport_rect.top() + 12.0),
        );
        let pos = Pos2::new(clamped_x, clamped_y);
        let mut action = None;

        Area::new(Id::unique("tg_window_chat_preview"))
            .order(Order::Foreground)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(palette.window_bg)
                    .stroke(Stroke::new(1.0, palette.divider))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_min_size(card_size - Vec2::splat(20.0));
                        ui.set_max_size(card_size - Vec2::splat(20.0));

                        // Header: avatar + title/subtitle + close button.
                        ui.horizontal(|ui| {
                            let (av_rect, _) =
                                ui.allocate_exact_size(Vec2::splat(34.0), Sense::hover());
                            ui.painter().circle_filled(
                                av_rect.center(),
                                17.0,
                                avatar_color(data.color_index),
                            );
                            ui.painter().text(
                                av_rect.center(),
                                Align2::CENTER_CENTER,
                                &data.initials,
                                font_id_from_style(st::semibold_font),
                                Color32::WHITE,
                            );

                            ui.vertical(|ui| {
                                ui.label(
                                    egui::RichText::new(&data.title)
                                        .font(font_id_from_style(st::semibold_font))
                                        .color(palette.window_fg),
                                );
                                ui.label(
                                    egui::RichText::new(&data.subtitle)
                                        .font(font_id_from_style(st::normal_font))
                                        .color(palette.window_sub_text_fg),
                                );
                            });

                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("Close").clicked() {
                                        action = Some(ChatPreviewAction::Closed);
                                    }
                                },
                            );
                        });

                        ui.separator();

                        // Scrollable message preview list.
                        let msg_h = (card_size.y - 126.0).max(80.0);
                        ScrollArea::vertical()
                            .id_salt(("chat_preview_msgs", data.peer_id.0))
                            .max_height(msg_h)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                if data.messages.is_empty() {
                                    ui.label(
                                        egui::RichText::new("No messages yet")
                                            .color(palette.window_sub_text_fg),
                                    );
                                } else {
                                    for msg in &data.messages {
                                        let bubble_bg = if msg.outgoing {
                                            palette.msg_out_bg
                                        } else {
                                            palette.msg_in_bg
                                        };
                                        egui::Frame::NONE
                                            .fill(bubble_bg)
                                            .corner_radius(CornerRadius::same(8))
                                            .inner_margin(egui::Margin::symmetric(8, 5))
                                            .show(ui, |ui| {
                                                if !msg.sender.is_empty() && !msg.outgoing {
                                                    ui.label(
                                                        egui::RichText::new(&msg.sender)
                                                            .font(font_id_from_style(
                                                                st::semibold_font,
                                                            ))
                                                            .color(palette.window_active_text_fg),
                                                    );
                                                }
                                                ui.horizontal_wrapped(|ui| {
                                                    ui.label(
                                                        egui::RichText::new(&msg.text)
                                                            .font(font_id_from_style(
                                                                st::normal_font,
                                                            ))
                                                            .color(palette.window_fg),
                                                    );
                                                    ui.label(
                                                        egui::RichText::new(&msg.time_text)
                                                            .size(11.0)
                                                            .color(palette.window_sub_text_fg),
                                                    );
                                                });
                                            });
                                        ui.add_space(4.0);
                                    }
                                }
                            });

                        ui.separator();

                        // Quick action bar.
                        ui.horizontal(|ui| {
                            if ui.button("Open Chat").clicked() {
                                action = Some(ChatPreviewAction::OpenChat(data.peer_id));
                            }
                            if ui.button("New Window").clicked() {
                                action = Some(ChatPreviewAction::OpenSeparateWindow(data.peer_id));
                            }
                            if data.unread_count > 0 && ui.button("Mark Read").clicked() {
                                action = Some(ChatPreviewAction::MarkRead(data.peer_id));
                            }
                            let mute_lbl = if data.muted { "Unmute" } else { "Mute" };
                            if ui.button(mute_lbl).clicked() {
                                action = Some(ChatPreviewAction::ToggleMute(data.peer_id));
                            }
                        });
                    });
            });

        if action.is_some() {
            self.close();
        }
        action
    }
}

/// Entry displayed in the `Ctrl+Tab` chat-switch overlay (`Window::ChatSwitchProcess`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatSwitchEntry {
    /// Target peer ID.
    pub peer_id: PeerId,
    /// Display title.
    pub title: String,
    /// Two-letter avatar monogram.
    pub initials: String,
    /// Avatar color index.
    pub color_index: u8,
    /// Unread counter badge.
    pub unread_count: u32,
}

/// Result emitted by [`ChatSwitchOverlay::show`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatSwitchAction {
    /// User confirmed switching to `PeerId` (released Ctrl, pressed Enter, or clicked tile).
    Choose(PeerId),
    /// User closed `PeerId` from the switcher (`Q` key).
    CloseChat(PeerId),
    /// User cancelled switching (`Esc`).
    Cancel,
}

/// Stateful `Ctrl+Tab` recent-chats switcher (`Window::ChatSwitchProcess`).
#[derive(Debug, Default)]
pub struct ChatSwitchOverlay {
    active: bool,
    entries: Vec<ChatSwitchEntry>,
    selected: usize,
}

impl ChatSwitchOverlay {
    /// Creates an inactive [`ChatSwitchOverlay`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            active: false,
            entries: Vec::new(),
            selected: 0,
        }
    }

    /// Returns `true` when the overlay is currently visible.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.active
    }

    /// Returns the currently highlighted entry index.
    #[must_use]
    pub const fn selected_index(&self) -> usize {
        self.selected
    }

    /// Returns the currently highlighted [`ChatSwitchEntry`], if any.
    #[must_use]
    pub fn selected_entry(&self) -> Option<&ChatSwitchEntry> {
        self.entries.get(self.selected)
    }

    /// Opens the switcher populated with `entries`, highlighting the next (or previous if `reverse`)
    /// chat relative to `active_peer`.
    pub fn open(
        &mut self,
        entries: Vec<ChatSwitchEntry>,
        active_peer: Option<PeerId>,
        reverse: bool,
    ) {
        if entries.is_empty() {
            self.active = false;
            self.entries.clear();
            self.selected = 0;
            return;
        }
        let len = entries.len();
        let current_idx = active_peer
            .and_then(|p| entries.iter().position(|e| e.peer_id == p))
            .unwrap_or(0);
        self.selected = if len == 1 {
            0
        } else if reverse {
            (current_idx + len - 1) % len
        } else {
            (current_idx + 1) % len
        };
        self.entries = entries;
        self.active = true;
    }

    /// Advances selection forward (`Ctrl+Tab`) or backward (`Ctrl+Shift+Tab`).
    pub fn step(&mut self, reverse: bool) {
        if self.entries.is_empty() {
            return;
        }
        let len = self.entries.len();
        if reverse {
            self.selected = (self.selected + len - 1) % len;
        } else {
            self.selected = (self.selected + 1) % len;
        }
    }

    /// Moves selection in the 2D tile grid using arrow keys.
    pub fn move_grid(&mut self, dx: isize, dy: isize) {
        let len = self.entries.len();
        if len == 0 {
            return;
        }
        let cols = CHAT_SWITCH_COLUMNS.min(len);
        let cur = self.selected as isize;
        let next = cur + dx + dy * (cols as isize);
        self.selected = next.rem_euclid(len as isize) as usize;
    }

    /// Confirms the highlighted entry and closes the overlay.
    pub fn confirm(&mut self) -> Option<PeerId> {
        if !self.active {
            return None;
        }
        let peer = self.entries.get(self.selected).map(|e| e.peer_id);
        self.cancel();
        peer
    }

    /// Cancels and hides the overlay.
    pub fn cancel(&mut self) {
        self.active = false;
        self.entries.clear();
        self.selected = 0;
    }

    /// Renders the centered `Ctrl+Tab` chat-switch overlay and processes keyboard/mouse actions.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        viewport_rect: Rect,
        palette: &Palette,
    ) -> Option<ChatSwitchAction> {
        if !self.active || self.entries.is_empty() {
            return None;
        }

        // Process keyboard navigation inside the switcher.
        let mut key_action = None;
        ctx.input(|i| {
            let ctrl = i.modifiers.ctrl || i.modifiers.command;
            if i.key_pressed(Key::Escape) {
                key_action = Some(ChatSwitchAction::Cancel);
            } else if i.key_pressed(Key::Enter)
                && let Some(entry) = self.entries.get(self.selected)
            {
                key_action = Some(ChatSwitchAction::Choose(entry.peer_id));
            } else if i.key_pressed(Key::Tab) {
                self.step(i.modifiers.shift);
            } else if i.key_pressed(Key::ArrowRight) {
                self.move_grid(1, 0);
            } else if i.key_pressed(Key::ArrowLeft) {
                self.move_grid(-1, 0);
            } else if i.key_pressed(Key::ArrowDown) {
                self.move_grid(0, 1);
            } else if i.key_pressed(Key::ArrowUp) {
                self.move_grid(0, -1);
            } else if ctrl
                && i.key_pressed(Key::Q)
                && let Some(entry) = self.entries.get(self.selected)
            {
                key_action = Some(ChatSwitchAction::CloseChat(entry.peer_id));
            }
        });

        if let Some(act) = key_action {
            match act {
                ChatSwitchAction::Cancel | ChatSwitchAction::Choose(_) => self.cancel(),
                ChatSwitchAction::CloseChat(peer) => {
                    self.entries.retain(|e| e.peer_id != peer);
                    if self.entries.is_empty() {
                        self.cancel();
                    } else if self.selected >= self.entries.len() {
                        self.selected = self.entries.len() - 1;
                    }
                }
            }
            return Some(act);
        }

        let mut click_action = None;
        let cols = CHAT_SWITCH_COLUMNS.min(self.entries.len()).max(1);
        let rows = self.entries.len().div_ceil(cols);
        let pad = 16.0;
        let gap = 8.0;
        let card_w = cols as f32 * CHAT_SWITCH_TILE_SIZE.x + (cols - 1) as f32 * gap + pad * 2.0;
        let card_h =
            rows as f32 * CHAT_SWITCH_TILE_SIZE.y + (rows - 1) as f32 * gap + pad * 2.0 + 28.0;
        let card_rect = Rect::from_center_size(viewport_rect.center(), Vec2::new(card_w, card_h));

        Area::new(Id::unique("tg_window_chat_switch_overlay"))
            .order(Order::Foreground)
            .fixed_pos(viewport_rect.min)
            .show(ctx, |ui| {
                let (_, bg_resp) = ui.allocate_exact_size(viewport_rect.size(), Sense::click());
                ui.painter().rect_filled(
                    viewport_rect,
                    CornerRadius::ZERO,
                    Color32::from_black_alpha(110),
                );
                if bg_resp.clicked() {
                    click_action = Some(ChatSwitchAction::Cancel);
                }

                ui.painter()
                    .rect_filled(card_rect, CornerRadius::same(12), palette.window_bg);
                ui.painter().rect_stroke(
                    card_rect,
                    CornerRadius::same(12),
                    Stroke::new(1.0, palette.divider),
                    StrokeKind::Inside,
                );

                ui.painter().text(
                    Pos2::new(card_rect.center().x, card_rect.top() + 18.0),
                    Align2::CENTER_CENTER,
                    "Switch Chat (Ctrl+Tab · Arrows · Enter · Esc)",
                    font_id_from_style(st::semibold_font),
                    palette.window_sub_text_fg,
                );

                for (idx, entry) in self.entries.iter().enumerate() {
                    let row = idx / cols;
                    let col = idx % cols;
                    let tile_min = Pos2::new(
                        card_rect.left() + pad + col as f32 * (CHAT_SWITCH_TILE_SIZE.x + gap),
                        card_rect.top()
                            + 32.0
                            + pad * 0.5
                            + row as f32 * (CHAT_SWITCH_TILE_SIZE.y + gap),
                    );
                    let tile_rect = Rect::from_min_size(tile_min, CHAT_SWITCH_TILE_SIZE);
                    let tile_resp = ui.interact(
                        tile_rect,
                        ui.scope_id().with(("chat_switch_tile", entry.peer_id.0)),
                        Sense::click(),
                    );
                    if tile_resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        self.selected = idx;
                    }
                    let is_selected = idx == self.selected;
                    if is_selected {
                        ui.painter().rect_filled(
                            tile_rect,
                            CornerRadius::same(8),
                            palette.window_bg_over,
                        );
                        ui.painter().rect_stroke(
                            tile_rect,
                            CornerRadius::same(8),
                            Stroke::new(2.0, palette.window_active_text_fg),
                            StrokeKind::Inside,
                        );
                    }

                    let avatar_center = Pos2::new(tile_rect.center().x, tile_rect.top() + 34.0);
                    ui.painter().circle_filled(
                        avatar_center,
                        22.0,
                        avatar_color(entry.color_index),
                    );
                    ui.painter().text(
                        avatar_center,
                        Align2::CENTER_CENTER,
                        &entry.initials,
                        font_id_from_style(st::semibold_font),
                        Color32::WHITE,
                    );

                    if entry.unread_count > 0 {
                        let badge_center =
                            Pos2::new(avatar_center.x + 17.0, avatar_center.y - 16.0);
                        ui.painter().circle_filled(
                            badge_center,
                            9.0,
                            palette.window_active_text_fg,
                        );
                        ui.painter().text(
                            badge_center,
                            Align2::CENTER_CENTER,
                            entry.unread_count.min(99).to_string(),
                            egui::FontId::proportional(10.0),
                            Color32::WHITE,
                        );
                    }

                    let truncated = truncate_label(&entry.title, 12);
                    ui.painter().text(
                        Pos2::new(tile_rect.center().x, tile_rect.bottom() - 16.0),
                        Align2::CENTER_CENTER,
                        truncated,
                        font_id_from_style(st::normal_font),
                        palette.window_fg,
                    );

                    if tile_resp.clicked() {
                        click_action = Some(ChatSwitchAction::Choose(entry.peer_id));
                    }
                }
            });

        if let Some(act) = click_action {
            self.cancel();
            return Some(act);
        }

        None
    }
}

/// Section kind inside a separate saved chat window (`Window::SavedChatSection`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SavedChatSection {
    #[default]
    Chat,
    Pinned,
    Scheduled,
    AdminLog,
}

/// Persistent separate chat window descriptor (`Window::SavedWindow`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedChatWindow {
    /// Target chat peer ID.
    pub peer_id: i64,
    /// Display window title.
    pub title: String,
    /// Section opened in the separate window.
    pub section: SavedChatSection,
    /// Optional topic root message ID for forum topics.
    pub topic_root_id: i32,
    /// Last known window width in logical pixels.
    pub width: f32,
    /// Last known window height in logical pixels.
    pub height: f32,
    /// Draft compose text inside this separate viewport.
    #[serde(default)]
    pub draft_text: String,
}

impl SavedChatWindow {
    /// Creates a separate chat window descriptor with default geometry (`520x640`).
    #[must_use]
    pub fn new(peer_id: PeerId, title: impl Into<String>) -> Self {
        Self {
            peer_id: peer_id.0,
            title: title.into(),
            section: SavedChatSection::Chat,
            topic_root_id: 0,
            width: 520.0,
            height: 640.0,
            draft_text: String::new(),
        }
    }

    /// Returns the deterministic [`ViewportId`] for this separate chat window.
    #[must_use]
    pub fn viewport_id(&self) -> ViewportId {
        ViewportId::from_hash_of((
            "rustgram_separate_chat_window",
            self.peer_id,
            self.topic_root_id,
        ))
    }
}

/// Action emitted from a separate chat viewport window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeparateWindowAction {
    /// User sent a text message from a separate chat window.
    SendMessage {
        /// Target peer ID.
        peer_id: PeerId,
        /// Message body text.
        text: String,
    },
    /// User closed the separate window.
    Closed(PeerId),
}

/// Manages open separate chat viewports, persistence (`saved-windows.json`), and recently closed
/// window stack (`Ctrl+Shift+T` / `Window::SavedWindows`).
#[derive(Debug, Default)]
pub struct SavedWindowsManager {
    open: Vec<SavedChatWindow>,
    closed: VecDeque<SavedChatWindow>,
    restore_on_launch: bool,
}

impl SavedWindowsManager {
    /// Creates an empty [`SavedWindowsManager`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether separate windows are automatically restored on launch.
    #[must_use]
    pub const fn restore_on_launch(&self) -> bool {
        self.restore_on_launch
    }

    /// Sets the `restore_on_launch` preference.
    pub fn set_restore_on_launch(&mut self, restore: bool) {
        self.restore_on_launch = restore;
    }

    /// Returns currently open separate chat windows.
    #[must_use]
    pub fn open_windows(&self) -> &[SavedChatWindow] {
        &self.open
    }

    /// Opens (or focuses if already open) a separate chat window for `window`.
    pub fn open_window(&mut self, window: SavedChatWindow) {
        if self.open.iter().any(|w| w.peer_id == window.peer_id) {
            return;
        }
        if self.open.len() >= MAX_SAVED_WINDOWS {
            return;
        }
        self.open.push(window);
    }

    /// Closes the separate window for `peer_id` and pushes it onto the reopen stack (`Ctrl+Shift+T`).
    pub fn close_window(&mut self, peer_id: PeerId) -> bool {
        if let Some(pos) = self.open.iter().position(|w| w.peer_id == peer_id.0) {
            let removed = self.open.remove(pos);
            self.closed.push_back(removed);
            while self.closed.len() > MAX_CLOSED_WINDOWS {
                self.closed.pop_front();
            }
            return true;
        }
        false
    }

    /// Reopens the most recently closed separate chat window (`Command::ReopenClosedWindow`).
    pub fn reopen_last_closed(&mut self) -> Option<PeerId> {
        let win = self.closed.pop_back()?;
        let peer_id = PeerId(win.peer_id);
        self.open_window(win);
        Some(peer_id)
    }

    /// Closes all separate windows (`Command::CloseOtherWindows`).
    pub fn close_all(&mut self) {
        for win in self.open.drain(..) {
            self.closed.push_back(win);
            while self.closed.len() > MAX_CLOSED_WINDOWS {
                self.closed.pop_front();
            }
        }
    }

    /// Serializes open separate windows to JSON.
    pub fn serialize_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(&self.open)
            .map_err(|e| format!("failed to serialize saved windows: {e}"))
    }

    /// Restores saved windows from JSON.
    pub fn load_json(&mut self, json: &str) -> Result<(), String> {
        let mut parsed: Vec<SavedChatWindow> = serde_json::from_str(json)
            .map_err(|e| format!("failed to parse saved windows: {e}"))?;
        parsed.truncate(MAX_SAVED_WINDOWS);
        self.open = parsed;
        Ok(())
    }

    /// Persists open separate windows to `<tdata_dir>/saved-windows.json`.
    pub fn save_to_tdata_dir(&self, tdata_dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(tdata_dir)
            .map_err(|e| format!("failed to create {}: {e}", tdata_dir.display()))?;
        let json = self.serialize_json()?;
        std::fs::write(tdata_dir.join(SAVED_WINDOWS_FILENAME), json)
            .map_err(|e| format!("failed to write saved windows: {e}"))
    }

    /// Loads open separate windows from `<tdata_dir>/saved-windows.json` if present.
    pub fn load_from_tdata_dir(&mut self, tdata_dir: &Path) -> Result<(), String> {
        let path = tdata_dir.join(SAVED_WINDOWS_FILENAME);
        if !path.exists() {
            return Ok(());
        }
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        self.load_json(&raw)
    }

    /// Renders every open separate chat window in its own `egui` immediate viewport.
    pub fn show_viewports<F>(
        &mut self,
        ctx: &egui::Context,
        palette: &Palette,
        mut fetch_messages: F,
    ) -> Vec<SeparateWindowAction>
    where
        F: FnMut(PeerId) -> Vec<ChatPreviewMessage>,
    {
        let mut actions = Vec::new();
        let mut closed_peers = Vec::new();

        for win in &mut self.open {
            let peer_id = PeerId(win.peer_id);
            let viewport_id = win.viewport_id();
            let builder = ViewportBuilder::default()
                .with_title(format!("{} — Rustgram", win.title))
                .with_inner_size([win.width, win.height])
                .with_min_inner_size([320.0, 360.0]);
            let messages = fetch_messages(peer_id);

            ctx.show_viewport_immediate(viewport_id, builder, |vp_ui, _| {
                if vp_ui.ctx().input(|i| i.viewport().close_requested()) {
                    closed_peers.push(peer_id);
                    return;
                }

                egui::Panel::top(("sep_chat_header", win.peer_id))
                    .frame(
                        egui::Frame::NONE
                            .fill(palette.window_bg)
                            .inner_margin(egui::Margin::symmetric(12, 8)),
                    )
                    .show(vp_ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(&win.title)
                                    .font(font_id_from_style(st::semibold_font))
                                    .color(palette.window_fg),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("Close").clicked() {
                                        ui.ctx().send_viewport_cmd(ViewportCommand::Close);
                                        closed_peers.push(peer_id);
                                    }
                                },
                            );
                        });
                    });

                egui::Panel::bottom(("sep_chat_compose", win.peer_id))
                    .frame(
                        egui::Frame::NONE
                            .fill(palette.window_bg)
                            .inner_margin(egui::Margin::symmetric(10, 8)),
                    )
                    .show(vp_ui, |ui| {
                        ui.horizontal(|ui| {
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut win.draft_text)
                                    .hint_text("Write a message...")
                                    .desired_width((ui.available_width() - 64.0).max(120.0)),
                            );
                            let send_enter =
                                resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                            if (ui.button("Send").clicked() || send_enter)
                                && !win.draft_text.trim().is_empty()
                            {
                                let text = std::mem::take(&mut win.draft_text);
                                actions.push(SeparateWindowAction::SendMessage { peer_id, text });
                            }
                        });
                    });

                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::NONE
                            .fill(palette.chat_canvas_bg)
                            .inner_margin(egui::Margin::same(10)),
                    )
                    .show(vp_ui, |ui| {
                        ScrollArea::vertical()
                            .id_salt(("sep_chat_scroll", win.peer_id))
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                for msg in &messages {
                                    let bg = if msg.outgoing {
                                        palette.msg_out_bg
                                    } else {
                                        palette.msg_in_bg
                                    };
                                    egui::Frame::NONE
                                        .fill(bg)
                                        .corner_radius(CornerRadius::same(8))
                                        .inner_margin(egui::Margin::symmetric(10, 6))
                                        .show(ui, |ui| {
                                            if !msg.sender.is_empty() && !msg.outgoing {
                                                ui.label(
                                                    egui::RichText::new(&msg.sender)
                                                        .font(font_id_from_style(st::semibold_font))
                                                        .color(palette.window_active_text_fg),
                                                );
                                            }
                                            ui.label(
                                                egui::RichText::new(&msg.text)
                                                    .color(palette.window_fg),
                                            );
                                        });
                                    ui.add_space(6.0);
                                }
                            });
                    });
            });
        }

        for peer in closed_peers {
            if self.close_window(peer) {
                actions.push(SeparateWindowAction::Closed(peer));
            }
        }
        actions
    }
}

/// Custom frameless Win32 title bar + Aero snap controller usable when `eframe` runs with
/// `with_decorations(false)` (`main_window_win.cpp`).
#[derive(Debug, Default)]
pub struct FramelessTitleBarState {
    last_dark: Option<bool>,
    aero_applied: bool,
}

impl FramelessTitleBarState {
    /// Synchronizes Win32 DWM dark mode and optional Aero-snap styles on `hwnd`.
    pub fn sync_win32_frame(&mut self, hwnd: isize, dark: bool, frameless: bool) {
        if hwnd == 0 {
            return;
        }
        if self.last_dark != Some(dark) {
            let _ = apply_window_dark_mode(hwnd, dark);
            self.last_dark = Some(dark);
        }
        if frameless && !self.aero_applied {
            let _ = apply_frameless_aero_snap(hwnd, true);
            self.aero_applied = true;
        }
    }

    /// Renders tdesktop's custom frameless window title bar (`Ui::Platform::TitleWidget`,
    /// `st::defaultWindowTitle`): a 24 px `titleBg` strip with no caption text, drag and
    /// double-click maximize, resize edge triggers (`ViewportCommand::BeginResize`) and the
    /// 36 px Minimize / Maximize / Close buttons.
    ///
    /// The strip is painted on its own `Order::Middle` layer; its last pixel row is the
    /// near-transparent `titleShadow` (`#00000003`) line, as `Ui::Platform::TitleWidget` paints it.
    pub fn show_title_bar(&mut self, ui: &mut Ui, title: &str, palette: &Palette) {
        let ctx = ui.ctx().clone();
        let viewport_rect = ctx.viewport_rect();
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        let controls_w = WINDOW_TITLE_BUTTON_WIDTH * 3.0;

        // Handle outer border resize hit-testing when not maximized.
        if let Some(pointer) = ctx.input(|i| i.pointer.hover_pos()) {
            let zone = hit_test_frameless(
                pointer.x - viewport_rect.left(),
                pointer.y - viewport_rect.top(),
                viewport_rect.width(),
                viewport_rect.height(),
                maximized,
                DEFAULT_RESIZE_BORDER_PX,
                WINDOW_TITLE_HEIGHT,
                controls_w,
            );
            if let FramelessHitZone::Resize(edge) = zone {
                let (cursor, dir) = map_resize_edge(edge);
                ctx.set_cursor_icon(cursor);
                if ctx.input(|i| i.pointer.primary_pressed()) {
                    ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
                }
            }
        }

        let bar_rect = Rect::from_min_size(
            ui.max_rect().min,
            Vec2::new(ui.available_width().max(120.0), WINDOW_TITLE_HEIGHT),
        );
        let (_, _) = ui.allocate_exact_size(bar_rect.size(), Sense::hover());
        let painter = ctx
            .layer_painter(egui::LayerId::new(
                Order::Middle,
                Id::unique("tg_frameless_title_paint"),
            ))
            .with_clip_rect(bar_rect);
        let colors = TitleColors::from_palette(palette);
        painter.rect_filled(bar_rect, CornerRadius::ZERO, colors.bg);
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(bar_rect.left(), bar_rect.bottom() - 1.0),
                bar_rect.max,
            ),
            CornerRadius::ZERO,
            TITLE_SHADOW,
        );

        let drag_rect = Rect::from_min_max(
            bar_rect.min,
            Pos2::new(
                (bar_rect.right() - controls_w).max(bar_rect.left()),
                bar_rect.max.y,
            ),
        );
        let drag_resp = ui.interact(
            drag_rect,
            ui.make_persistent_id("tg_frameless_caption"),
            Sense::click_and_drag(),
        );
        drag_resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, title));
        if drag_resp.drag_started() {
            ctx.send_viewport_cmd(ViewportCommand::StartDrag);
        }
        if drag_resp.double_clicked() {
            ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
        }

        let buttons = [
            ("Minimize", 0usize),
            (if maximized { "Restore" } else { "Maximize" }, 1usize),
            ("Close", 2usize),
        ];
        for (aria_label, idx) in buttons {
            let btn_rect = Rect::from_min_size(
                Pos2::new(
                    bar_rect.right() - WINDOW_TITLE_BUTTON_WIDTH * (3 - idx) as f32,
                    bar_rect.top(),
                ),
                Vec2::new(WINDOW_TITLE_BUTTON_WIDTH, bar_rect.height()),
            );
            let resp = ui.interact(
                btn_rect,
                ui.make_persistent_id(("tg_frameless_btn", idx)),
                Sense::click(),
            );
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, aria_label));
            let close = idx == 2;
            if resp.hovered() {
                let hover_bg = if close {
                    colors.close_bg_over
                } else {
                    colors.button_bg_over
                };
                painter.rect_filled(btn_rect, CornerRadius::ZERO, hover_bg);
            }
            let fg = match (close, resp.hovered()) {
                (true, true) => colors.close_fg_over,
                (false, true) => colors.button_fg_over,
                _ => colors.button_fg,
            };
            paint_title_glyph(&painter, btn_rect.center(), idx, maximized, fg);
            if resp.clicked() {
                match idx {
                    0 => ctx.send_viewport_cmd(ViewportCommand::Minimized(true)),
                    1 => ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized)),
                    _ => ctx.send_viewport_cmd(ViewportCommand::Close),
                }
            }
        }
    }
}

/// Window title bar height (`st::windowTitleHeight`).
pub const WINDOW_TITLE_HEIGHT: f32 = 24.0;
/// `titleShadow`: the one pixel line at the bottom of the custom window title (`#00000003`).
pub const TITLE_SHADOW: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 3);
/// Window title control button width (`st::windowTitleButtonWidth`).
pub const WINDOW_TITLE_BUTTON_WIDTH: f32 = 36.0;

/// `titleBg` / `titleButton*` colors (`colors.palette`) derived from the shell palette:
/// `titleBg` is `windowBgOver` (#F1F1F1 Day), buttons use `titleButtonFg` (#ababab) on
/// `titleButtonBgOver` (#e5e5e5) with the red `titleButtonCloseBgOver` (#e81123).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TitleColors {
    /// `titleBg` / `titleBgActive`.
    pub bg: Color32,
    /// `titleButtonFg`.
    pub button_fg: Color32,
    /// `titleButtonBgOver`.
    pub button_bg_over: Color32,
    /// `titleButtonFgOver`.
    pub button_fg_over: Color32,
    /// `titleButtonCloseBgOver`.
    pub close_bg_over: Color32,
    /// `titleButtonCloseFgOver` (`windowFgActive`).
    pub close_fg_over: Color32,
}

impl TitleColors {
    /// Resolves title colors from the shell palette.
    #[must_use]
    pub fn from_palette(palette: &Palette) -> Self {
        let bg = palette.window_bg_over;
        Self {
            bg,
            button_fg: mix(palette.window_sub_text_fg, bg, 0.25),
            button_bg_over: mix(bg, palette.window_fg, 0.05),
            button_fg_over: palette.window_sub_text_fg,
            close_bg_over: Color32::from_rgb(0xE8, 0x11, 0x23),
            close_fg_over: Color32::WHITE,
        }
    }
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let lerp = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(lerp(a.r(), b.r()), lerp(a.g(), b.g()), lerp(a.b(), b.b()))
}

/// Draws the 10 px `title_button_minimize` / `_maximize` / `_restore` / `_close` glyphs.
fn paint_title_glyph(painter: &egui::Painter, c: Pos2, idx: usize, maximized: bool, fg: Color32) {
    let stroke = Stroke::new(1.0, fg);
    let c = Pos2::new(c.x.round() + 0.5, c.y.round() + 0.5);
    match idx {
        0 => {
            painter.line_segment(
                [Pos2::new(c.x - 5.0, c.y), Pos2::new(c.x + 5.0, c.y)],
                stroke,
            );
        }
        1 if maximized => {
            let front = Rect::from_min_size(Pos2::new(c.x - 5.0, c.y - 3.0), Vec2::splat(8.0));
            painter.rect_stroke(front, CornerRadius::ZERO, stroke, StrokeKind::Middle);
            painter.line_segment(
                [
                    Pos2::new(c.x - 3.0, c.y - 5.0),
                    Pos2::new(c.x + 5.0, c.y - 5.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(c.x + 5.0, c.y - 5.0),
                    Pos2::new(c.x + 5.0, c.y + 3.0),
                ],
                stroke,
            );
        }
        1 => {
            let sq = Rect::from_center_size(c, Vec2::splat(10.0));
            painter.rect_stroke(sq, CornerRadius::ZERO, stroke, StrokeKind::Middle);
        }
        _ => {
            let r = 5.0;
            painter.line_segment(
                [Pos2::new(c.x - r, c.y - r), Pos2::new(c.x + r, c.y + r)],
                stroke,
            );
            painter.line_segment(
                [Pos2::new(c.x - r, c.y + r), Pos2::new(c.x + r, c.y - r)],
                stroke,
            );
        }
    }
}

const fn map_resize_edge(edge: ResizeEdge) -> (CursorIcon, ResizeDirection) {
    match edge {
        ResizeEdge::North => (CursorIcon::ResizeVertical, ResizeDirection::North),
        ResizeEdge::South => (CursorIcon::ResizeVertical, ResizeDirection::South),
        ResizeEdge::West => (CursorIcon::ResizeHorizontal, ResizeDirection::West),
        ResizeEdge::East => (CursorIcon::ResizeHorizontal, ResizeDirection::East),
        ResizeEdge::NorthWest => (CursorIcon::ResizeNwSe, ResizeDirection::NorthWest),
        ResizeEdge::SouthEast => (CursorIcon::ResizeNwSe, ResizeDirection::SouthEast),
        ResizeEdge::NorthEast => (CursorIcon::ResizeNeSw, ResizeDirection::NorthEast),
        ResizeEdge::SouthWest => (CursorIcon::ResizeNeSw, ResizeDirection::SouthWest),
    }
}

fn avatar_color(index: u8) -> Color32 {
    const PALETTE: [Color32; 7] = [
        Color32::from_rgb(0xE1, 0x70, 0x76),
        Color32::from_rgb(0xFA, 0xA7, 0x74),
        Color32::from_rgb(0xA6, 0x95, 0xE7),
        Color32::from_rgb(0x7B, 0xC8, 0x62),
        Color32::from_rgb(0x6E, 0xC9, 0xCB),
        Color32::from_rgb(0x65, 0xAA, 0xDD),
        Color32::from_rgb(0xEE, 0x7A, 0xAE),
    ];
    PALETTE[(index as usize) % PALETTE.len()]
}

fn truncate_label(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

/// Composite state bundle mounted in `WindowController` with minimal registration footprint.
///
/// Live-window glue for saved separate chat windows (`Window::SavedWindows::startRestore`,
/// `ShowRestoreOffer` and the save-on-change path in `window_saved_windows.cpp`).
///
/// Loads `tdata/restore_windows.json` once per launch, restores windows according to the policy
/// (`Always` reopens them, `Ask` shows the "Restore windows?" card), and writes the set of open
/// separate windows back whenever it changes.
#[derive(Debug, Default)]
pub struct RestoreWindowsHost {
    working_dir: Option<std::path::PathBuf>,
    manager: tg_app_core::core_types::restore_windows::RestoreWindowsManager,
    last_saved: Vec<(i64, String)>,
}

impl RestoreWindowsHost {
    /// Whether [`Self::install`] has run.
    #[must_use]
    pub const fn is_installed(&self) -> bool {
        self.working_dir.is_some()
    }

    /// Whether the "Restore windows?" card is waiting for an answer.
    #[must_use]
    pub const fn has_pending_offer(&self) -> bool {
        self.manager.pending_offer.is_some()
    }

    /// Current restore policy.
    #[must_use]
    pub const fn policy(&self) -> tg_app_core::core_types::restore_windows::RestoreWindowsPolicy {
        self.manager.policy
    }

    /// Reads the saved snapshot under `working_dir` and starts the restore (once per launch).
    pub fn install(&mut self, working_dir: &std::path::Path, saved: &mut SavedWindowsManager) {
        use tg_app_core::core_types::restore_windows::{
            RestoreWindowsManager, SavedWindowsSnapshot, ScreenBounds,
        };
        if self.is_installed() {
            return;
        }
        let snapshot = SavedWindowsSnapshot::load_from(working_dir);
        self.manager = RestoreWindowsManager::from_snapshot(snapshot, ScreenBounds::default());
        self.working_dir = Some(working_dir.to_path_buf());
        self.open_restored(saved);
        self.last_saved = Self::open_keys(saved);
    }

    /// Applies a choice on the restore card, returning a toast to show.
    pub fn choose(
        &mut self,
        action: tg_app_core::core_types::restore_windows::RestoreWindowsAction,
        saved: &mut SavedWindowsManager,
    ) -> Option<String> {
        use tg_app_core::core_types::restore_windows::{
            RestoreWindowsAction, SavedWindowsSnapshot, ScreenBounds,
        };
        let toast = self
            .manager
            .apply_offer_action(action, ScreenBounds::default());
        self.open_restored(saved);
        if matches!(
            action,
            RestoreWindowsAction::Always | RestoreWindowsAction::Never
        ) && let Some(dir) = self.working_dir.as_deref()
        {
            let mut snap = SavedWindowsSnapshot::load_from(dir);
            snap.policy = self.manager.policy;
            let _ = snap.save_to(dir);
        }
        toast
    }

    /// Renders the restore card at the top right of the window and saves the open windows when
    /// they change. Opening a chat dismisses the card, as in `ShowRestoreOffer`.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        saved: &mut SavedWindowsManager,
        chat_open: bool,
    ) -> Option<String> {
        use tg_app_core::core_types::restore_windows::RestoreWindowsAction;
        if !self.is_installed() {
            return None;
        }
        let mut toast = None;
        if self.has_pending_offer() {
            let chosen = if chat_open {
                Some(RestoreWindowsAction::Dismiss)
            } else {
                egui::Area::new(egui::Id::unique("rustgram_restore_windows_offer"))
                    .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-12.0, 56.0))
                    .order(egui::Order::Foreground)
                    .show(ctx, |ui| {
                        ui.set_max_width(360.0);
                        let picked = self.manager.render_offer_banner(ui);
                        let close = ui
                            .small_button("\u{2715}")
                            .on_hover_text(tg_lang::tr::lng_close())
                            .clicked();
                        picked.or(close.then_some(RestoreWindowsAction::Dismiss))
                    })
                    .inner
            };
            if let Some(action) = chosen {
                toast = self.choose(action, saved);
            }
        }
        self.save_if_changed(saved);
        toast
    }

    /// Writes the open separate windows to `restore_windows.json` if they changed since the last
    /// write, keeping the policy stored on disk (the Advanced settings row may have changed it).
    pub fn save_if_changed(&mut self, saved: &SavedWindowsManager) {
        use tg_app_core::core_types::restore_windows::{
            SavedWindowEntry, SavedWindowGeometry, SavedWindowsSnapshot,
        };
        let Some(dir) = self.working_dir.as_deref() else {
            return;
        };
        let keys = Self::open_keys(saved);
        if keys == self.last_saved {
            return;
        }
        let mut snap = SavedWindowsSnapshot::load_from(dir);
        snap.windows = saved
            .open_windows()
            .iter()
            .enumerate()
            .map(|(idx, w)| SavedWindowEntry {
                id: idx as u64 + 1,
                title: w.title.clone(),
                account_index: 0,
                peer_id: Some(w.peer_id),
                position: SavedWindowGeometry {
                    w: w.width.round() as i32,
                    h: w.height.round() as i32,
                    ..SavedWindowGeometry::default()
                },
            })
            .collect();
        if snap.save_to(dir).is_ok() {
            self.last_saved = keys;
        }
    }

    fn open_keys(saved: &SavedWindowsManager) -> Vec<(i64, String)> {
        saved
            .open_windows()
            .iter()
            .map(|w| (w.peer_id, w.title.clone()))
            .collect()
    }

    /// Turns every spawned `RestoreShell` into a separate chat window.
    fn open_restored(&mut self, saved: &mut SavedWindowsManager) {
        use tg_app_core::core_types::restore_windows::{
            WINDOW_SHELL_MIN_HEIGHT, WINDOW_SHELL_MIN_WIDTH,
        };
        for mut shell in std::mem::take(&mut self.manager.shells) {
            match shell.peer_id {
                Some(peer) => {
                    let mut window = SavedChatWindow::new(PeerId(peer), shell.title.clone());
                    let (_, _, w, h) = shell.normal_body;
                    window.width = w.max(WINDOW_SHELL_MIN_WIDTH) as f32;
                    window.height = h.max(WINDOW_SHELL_MIN_HEIGHT) as f32;
                    saved.open_window(window);
                    shell.mark_ready();
                }
                None => {
                    shell.show_unavailable();
                    self.manager.dead_shells.push(shell);
                }
            }
        }
    }
}

/// Combines hover/shortcut chat previews (`window_chat_preview`), the `Ctrl+Tab` chat-switch
/// overlay (`window_chat_switch_process`), separate saved chat viewports (`window_saved_windows`),
/// and custom Win32 frameless title bar / DWM sync (`main_window_win`).
#[derive(Debug, Default)]
pub struct WindowShellExtras {
    /// Hover / `Ctrl+]` chat preview manager.
    pub chat_preview: ChatPreviewManager,
    /// `Ctrl+Tab` recent-chats switcher overlay.
    pub chat_switch: ChatSwitchOverlay,
    /// Separate saved chat windows manager.
    pub saved_windows: SavedWindowsManager,
    /// Optional Win32 frameless title bar & dark-mode DWM sync.
    pub frameless: FramelessTitleBarState,
    /// Saved-window restore offer and persistence (`window_restore_shell`, WP-40).
    pub restore: RestoreWindowsHost,
}

impl WindowShellExtras {
    /// Creates an idle [`WindowShellExtras`] bundle.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Handles `Ctrl+Tab` (chat switch overlay), `Ctrl+]` (chat preview), and `Ctrl+Shift+T`
    /// (reopen closed separate window). Returns `true` when the event was consumed.
    pub fn handle_shortcut_keys(
        &mut self,
        ctx: &egui::Context,
        dialogs: &[tg_app_core::stand_in::types::DialogEntry],
        active_peer: PeerId,
    ) -> bool {
        let (ctrl, alt, shift, tab, bracket_r, t) = ctx.input(|i| {
            (
                i.modifiers.ctrl || i.modifiers.command,
                i.modifiers.alt,
                i.modifiers.shift,
                i.key_pressed(Key::Tab),
                i.key_pressed(Key::CloseBracket),
                i.key_pressed(Key::T),
            )
        });

        if ctrl && !alt && tab && !dialogs.is_empty() {
            if self.chat_switch.is_active() {
                self.chat_switch.step(shift);
            } else {
                let entries: Vec<ChatSwitchEntry> = dialogs
                    .iter()
                    .take(15)
                    .map(|d| ChatSwitchEntry {
                        peer_id: d.peer.id,
                        title: d.peer.title.clone(),
                        initials: d.peer.initials.clone(),
                        color_index: tg_ui::userpic::decide_color_index(d.peer.id.bare()),
                        unread_count: d.unread_count,
                    })
                    .collect();
                self.chat_switch.open(entries, Some(active_peer), shift);
            }
            return true;
        }

        if ctrl && !alt && !shift && bracket_r && !active_peer.is_zero() {
            if self.chat_preview.is_open() {
                self.chat_preview.close();
            } else if let Some(d) = dialogs.iter().find(|d| d.peer.id == active_peer) {
                let data = ChatPreviewData {
                    peer_id: d.peer.id,
                    title: d.peer.title.clone(),
                    subtitle: d.peer.status.clone(),
                    initials: d.peer.initials.clone(),
                    color_index: tg_ui::userpic::decide_color_index(d.peer.id.bare()),
                    unread_count: d.unread_count,
                    muted: d.peer.muted,
                    messages: vec![ChatPreviewMessage {
                        sender: d.last_sender.clone().unwrap_or_default(),
                        text: d.last_message.clone(),
                        time_text: String::new(),
                        outgoing: false,
                    }],
                };
                self.chat_preview.show_data(data, Pos2::new(340.0, 96.0));
            }
            return true;
        }

        ctrl && !alt && shift && t && self.saved_windows.reopen_last_closed().is_some()
    }

    /// Renders active preview / switcher overlays and separate chat viewports.
    ///
    /// `dialogs` builds the main chat list. It is called only when a separate chat window opens
    /// (for its title) or is open (for its preview), so a frame without one never builds the list.
    ///
    /// Returns an optional `PeerId` to open in the main window if the user selected one.
    pub fn show_overlays(
        &mut self,
        ctx: &egui::Context,
        viewport_rect: Rect,
        palette: &Palette,
        dialogs: &mut dyn FnMut() -> std::sync::Arc<Vec<tg_app_core::stand_in::types::DialogEntry>>,
    ) -> Option<PeerId> {
        let mut open_peer = None;

        if let Some(act) = self.chat_preview.show(ctx, viewport_rect, palette) {
            match act {
                ChatPreviewAction::OpenChat(peer) => open_peer = Some(peer),
                ChatPreviewAction::OpenSeparateWindow(peer) => {
                    let title = dialogs()
                        .iter()
                        .find(|d| d.peer.id == peer)
                        .map_or_else(|| format!("Chat {}", peer.0), |d| d.peer.title.clone());
                    self.saved_windows
                        .open_window(SavedChatWindow::new(peer, title));
                }
                ChatPreviewAction::MarkRead(_)
                | ChatPreviewAction::ToggleMute(_)
                | ChatPreviewAction::Closed => {}
            }
        }

        if let Some(ChatSwitchAction::Choose(peer)) =
            self.chat_switch.show(ctx, viewport_rect, palette)
        {
            open_peer = Some(peer);
        }

        if !self.saved_windows.open_windows().is_empty() {
            let dialogs = dialogs();
            let _ = self.saved_windows.show_viewports(ctx, palette, |peer| {
                dialogs
                    .iter()
                    .find(|d| d.peer.id == peer)
                    .map(|d| {
                        vec![ChatPreviewMessage {
                            sender: d.last_sender.clone().unwrap_or_default(),
                            text: d.last_message.clone(),
                            time_text: String::new(),
                            outgoing: false,
                        }]
                    })
                    .unwrap_or_default()
            });
        }

        open_peer
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;
    use tg_app_core::stand_in::theme::ThemeMode;

    #[test]
    fn saved_windows_reopen_and_persist_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut mgr = SavedWindowsManager::new();
        mgr.open_window(SavedChatWindow::new(PeerId::from_user(10), "Saved Notes"));
        mgr.open_window(SavedChatWindow::new(
            PeerId::from_channel(20),
            "News Channel",
        ));
        assert_eq!(mgr.open_windows().len(), 2);

        assert!(mgr.close_window(PeerId::from_channel(20)));
        assert_eq!(mgr.open_windows().len(), 1);
        assert_eq!(mgr.reopen_last_closed(), Some(PeerId::from_channel(20)));
        assert_eq!(mgr.open_windows().len(), 2);

        mgr.save_to_tdata_dir(dir.path()).expect("save");
        let mut reloaded = SavedWindowsManager::new();
        reloaded.load_from_tdata_dir(dir.path()).expect("load");
        assert_eq!(reloaded.open_windows().len(), 2);
        assert_eq!(reloaded.open_windows()[0].title, "Saved Notes");
    }

    #[test]
    fn kittest_chat_preview_renders_and_opens_chat() {
        let palette = Palette::for_mode(ThemeMode::Day);
        let mut manager = ChatPreviewManager::new();
        manager.schedule(PeerId::from_user(42), Pos2::new(120.0, 80.0), 1.0);
        assert!(manager.poll_scheduled(1.2).is_none());
        let (peer, anchor) = manager.poll_scheduled(1.5).expect("hover timer elapsed");
        assert_eq!(peer, PeerId::from_user(42));

        manager.show_data(
            ChatPreviewData {
                peer_id: peer,
                title: "Design Team".to_owned(),
                subtitle: "4 members".to_owned(),
                initials: "DT".to_owned(),
                color_index: 5,
                unread_count: 3,
                muted: false,
                messages: vec![
                    ChatPreviewMessage {
                        sender: "Lead".to_owned(),
                        text: "Updated the icon grid".to_owned(),
                        time_text: "10:15".to_owned(),
                        outgoing: false,
                    },
                    ChatPreviewMessage {
                        sender: String::new(),
                        text: "Looks great!".to_owned(),
                        time_text: "10:16".to_owned(),
                        outgoing: true,
                    },
                ],
            },
            anchor,
        );

        let mut harness = Harness::builder()
            .with_size(Vec2::new(640.0, 480.0))
            .allow_missing_glyphs()
            .with_accessibility_check(false)
            .with_context_setup(|ctx| {
                tg_ui::fonts::register_tdesktop_fonts(ctx);
            })
            .build_ui_state(
                |ui, state: &mut (ChatPreviewManager, Option<ChatPreviewAction>)| {
                    let ctx = ui.ctx().clone();
                    let vp = ctx.viewport_rect();
                    if let Some(act) = state.0.show(&ctx, vp, &palette) {
                        state.1 = Some(act);
                    }
                },
                (manager, None),
            );
        harness.run();

        assert!(harness.query_by_label("Design Team").is_some());
        assert!(harness.query_by_label("Updated the icon grid").is_some());
        harness.get_by_label("Open Chat").click();
        harness.run();

        assert_eq!(
            harness.state().1,
            Some(ChatPreviewAction::OpenChat(PeerId::from_user(42)))
        );
        assert!(!harness.state().0.is_open());
    }

    #[test]
    fn kittest_chat_switch_overlay_navigates_and_chooses_entry() {
        let palette = Palette::for_mode(ThemeMode::Day);
        let mut overlay = ChatSwitchOverlay::new();
        overlay.open(
            vec![
                ChatSwitchEntry {
                    peer_id: PeerId::from_user(1),
                    title: "Alpha".to_owned(),
                    initials: "AL".to_owned(),
                    color_index: 0,
                    unread_count: 0,
                },
                ChatSwitchEntry {
                    peer_id: PeerId::from_user(2),
                    title: "Beta".to_owned(),
                    initials: "BE".to_owned(),
                    color_index: 2,
                    unread_count: 4,
                },
                ChatSwitchEntry {
                    peer_id: PeerId::from_user(3),
                    title: "Gamma".to_owned(),
                    initials: "GA".to_owned(),
                    color_index: 4,
                    unread_count: 0,
                },
            ],
            Some(PeerId::from_user(1)),
            false,
        );
        // Starts at index 1 ("Beta") when PeerId(1) is active.
        assert_eq!(overlay.selected_index(), 1);

        let mut harness = Harness::builder()
            .with_size(Vec2::new(720.0, 480.0))
            .allow_missing_glyphs()
            .with_accessibility_check(false)
            .with_context_setup(|ctx| {
                tg_ui::fonts::register_tdesktop_fonts(ctx);
            })
            .build_ui_state(
                |ui, state: &mut (ChatSwitchOverlay, Option<ChatSwitchAction>)| {
                    let ctx = ui.ctx().clone();
                    let vp = ctx.viewport_rect();
                    if let Some(act) = state.0.show(&ctx, vp, &palette) {
                        state.1 = Some(act);
                    }
                },
                (overlay, None),
            );
        harness.run();
        assert!(harness.state().0.is_active());

        // Step once with Tab -> index 2 ("Gamma"), then press Enter -> Choose(PeerId::from_user(3)).
        harness.key_press(Key::Tab);
        harness.run();
        assert_eq!(harness.state().0.selected_index(), 2);

        harness.key_press(Key::Enter);
        harness.run();
        assert_eq!(
            harness.state().1,
            Some(ChatSwitchAction::Choose(PeerId::from_user(3)))
        );
        assert!(!harness.state().0.is_active());
    }

    /// The per-frame overlay pass must not build the chat list: it used to convert every chat
    /// of the account on every frame for a lookup that only separate chat windows need.
    #[test]
    fn overlays_build_the_chat_list_only_for_separate_windows() {
        use std::cell::Cell;

        let ctx = egui::Context::default();
        let palette = Palette::for_mode(ThemeMode::Day);
        let viewport = Rect::from_min_size(Pos2::ZERO, egui::vec2(1440.0, 900.0));
        let mut extras = WindowShellExtras::new();
        let built = Cell::new(0_u32);
        let mut dialogs = || {
            built.set(built.get() + 1);
            std::sync::Arc::new(Vec::new())
        };
        let mut run_frames = |extras: &mut WindowShellExtras, frames: usize| {
            for _ in 0..frames {
                let input = egui::RawInput {
                    screen_rect: Some(viewport),
                    ..Default::default()
                };
                ctx.run_ui(input, |ui| {
                    let _ = extras.show_overlays(ui.ctx(), viewport, &palette, &mut dialogs);
                })
                .drop_without_applying_deltas();
            }
        };

        run_frames(&mut extras, 60);
        assert_eq!(built.get(), 0, "idle frames must not build the chat list");

        extras
            .saved_windows
            .open_window(SavedChatWindow::new(PeerId::from_user(10), "Saved Notes"));
        run_frames(&mut extras, 3);
        assert_eq!(
            built.get(),
            3,
            "an open separate window reads the list once per frame"
        );
    }
}
