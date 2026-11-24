//! Custom in-app desktop notification popups used when native Windows toasts are disabled.
//!
//! Port of `Telegram/SourceFiles/window/notifications_manager_default.h`,
//! `Telegram/SourceFiles/window/notifications_manager_default.cpp`, and
//! `Telegram/SourceFiles/window/window.style` (`notify*` metrics).

use std::collections::VecDeque;
use std::sync::OnceLock;

use egui::{
    Align, Align2, Color32, CornerRadius, Frame, Layout, Margin, Order, Pos2, Rect, Sense, Stroke,
    Vec2,
};
use tg_platform::notifications::{ContextId, NotificationId};
use tg_ui::fonts::{ensure_tdesktop_fonts, font_id_from_style};
use tg_ui::icons::paint_style_icon;
use tg_ui::tg_style::st;
use tg_ui::tg_theme::{EmbeddedType, Palette as ThemePalette};
use tg_ui::theme::{Colors, ThemeManager};

use crate::notifications::settings::{NotificationPreferences, ScreenCorner};
use crate::notifications::strings;
use tg_app_core::stand_in::theme::Palette;

/// Notification card width (`st::notifyWidth = 320px`).
pub const NOTIFY_WIDTH: f32 = st::notify_width.0 as f32;
/// Base notification card height (`st::notifyMinHeight = 80px`).
pub const NOTIFY_MIN_HEIGHT: f32 = st::notify_min_height.0 as f32;
/// Avatar circle diameter (`st::notifyPhotoSize = 62px`).
pub const NOTIFY_PHOTO_SIZE: f32 = st::notify_photo_size.0 as f32;
/// Horizontal screen edge margin (`st::notifyDeltaX = 6px`).
pub const NOTIFY_DELTA_X: f32 = st::notify_delta_x.0 as f32;
/// Vertical gap between stacked notification cards (`st::notifyDeltaY = 7px`).
pub const NOTIFY_DELTA_Y: f32 = st::notify_delta_y.0 as f32;
/// Height of the "Hide all" bar (`st::notifyHideAllHeight = 36px`).
pub const NOTIFY_HIDE_ALL_HEIGHT: f32 = st::notify_hide_all_height.0 as f32;
/// Additional height when the inline reply composer is expanded.
pub const NOTIFY_REPLY_EXPANDED_HEIGHT: f32 = 46.0;
/// Wait duration before starting slow fade-out (`st::notifyWaitLongHide = 3000ms`).
pub const NOTIFY_WAIT_LONG_HIDE_MS: i64 = st::notify_wait_long_hide as i64;
/// Slow fade-out duration (`st::notifySlowHide = 4000ms`).
pub const NOTIFY_SLOW_HIDE_MS: i64 = st::notify_slow_hide as i64;
/// Fast show / dismiss animation duration (`st::notifyFastAnim = 150ms`).
pub const NOTIFY_FAST_ANIM_MS: i64 = st::notify_fast_anim as i64;

fn notify_theme_palette(palette: &Palette) -> &'static ThemePalette {
    static DAY: OnceLock<ThemePalette> = OnceLock::new();
    static NIGHT: OnceLock<ThemePalette> = OnceLock::new();
    if palette.window_bg.r() < 128 {
        NIGHT.get_or_init(|| {
            ThemeManager::with_embedded(EmbeddedType::Night)
                .palette()
                .clone()
        })
    } else {
        DAY.get_or_init(|| {
            ThemeManager::with_embedded(EmbeddedType::Default)
                .palette()
                .clone()
        })
    }
}

/// Stack expansion direction (`internal::Widget::Direction` in `notifications_manager_default.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StackDirection {
    /// Cards stack downward from a top corner (`TopLeft`, `TopRight`).
    Down,
    /// Cards stack upward from a bottom corner (`BottomLeft`, `BottomRight`).
    Up,
}

impl StackDirection {
    /// Maps a [`ScreenCorner`] to its stacking direction (`notificationShiftDirection`).
    #[must_use]
    pub const fn for_corner(corner: ScreenCorner) -> Self {
        if corner.is_top() {
            Self::Down
        } else {
            Self::Up
        }
    }
}

/// Content payload for creating a custom in-app popup notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomPopupContent {
    /// Unique `(context, msg_id)` identifier.
    pub id: NotificationId,
    /// Primary title line (chat name, topic + chat, or generic app name).
    pub title: String,
    /// Optional subtitle (e.g. group sender name).
    pub subtitle: Option<String>,
    /// Message preview text.
    pub message: String,
    /// Two-letter avatar initials.
    pub initials: String,
    /// Avatar circle RGB color.
    pub avatar_rgb: [u8; 3],
    /// Hide avatar & sender identity (`hideNameAndPhoto`).
    pub hide_name_and_photo: bool,
    /// Hide inline reply button (`hideReplyButton`).
    pub hide_reply_button: bool,
    /// Hide "Mark as read" button (`hideMarkAsRead`).
    pub hide_mark_as_read: bool,
}

/// User interaction emitted by [`CustomPopupManager`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PopupAction {
    /// User left-clicked the notification card to open the chat.
    Activate {
        /// Notification identifier.
        id: NotificationId,
        /// Unsent draft if user had typed in the reply box before clicking.
        draft: Option<String>,
    },
    /// User submitted an inline quick reply.
    Reply {
        /// Notification identifier.
        id: NotificationId,
        /// Reply message text.
        text: String,
    },
    /// User clicked "Mark as read".
    MarkAsRead(NotificationId),
    /// User dismissed a single notification card (close button or right-click).
    Dismiss(NotificationId),
    /// User clicked the "Hide all" bar.
    HideAll,
}

/// Individual live popup card state (`Default::internal::Notification`).
#[derive(Debug, Clone)]
pub struct PopupCard {
    /// Display content.
    pub content: CustomPopupContent,
    /// Timestamp (ms) when this card became visible.
    pub started_at_ms: i64,
    /// Timestamp (ms) when user input was detected and the hide countdown began.
    pub wait_started_at_ms: Option<i64>,
    /// Timestamp (ms) when slow or fast hide started.
    pub hiding_started_at_ms: Option<i64>,
    /// Duration (ms) of the active hide animation (`NOTIFY_SLOW_HIDE_MS` or `NOTIFY_FAST_ANIM_MS`).
    pub hiding_duration_ms: i64,
    /// Whether the inline reply input is currently expanded.
    pub replying: bool,
    /// Current inline reply draft text.
    pub reply_draft: String,
    /// Whether the card has been unlinked and is fading out (`_history == nullptr`).
    pub unlinked: bool,
    /// Whether the mouse pointer is currently over this card.
    pub hovered: bool,
    /// Computed opacity in `[0.0, 1.0]`.
    pub opacity: f32,
    /// Vertical offset from the anchor edge in logical pixels.
    pub shift_y: f32,
}

impl PopupCard {
    fn new(content: CustomPopupContent, now_ms: i64, wait_for_input: bool) -> Self {
        Self {
            content,
            started_at_ms: now_ms,
            wait_started_at_ms: if wait_for_input { None } else { Some(now_ms) },
            hiding_started_at_ms: None,
            hiding_duration_ms: NOTIFY_SLOW_HIDE_MS,
            replying: false,
            reply_draft: String::new(),
            unlinked: false,
            hovered: false,
            opacity: 1.0,
            shift_y: NOTIFY_DELTA_Y,
        }
    }

    /// Logical height of this card in pixels.
    #[must_use]
    pub fn height(&self) -> f32 {
        if self.replying {
            NOTIFY_MIN_HEIGHT + NOTIFY_REPLY_EXPANDED_HEIGHT
        } else {
            NOTIFY_MIN_HEIGHT
        }
    }

    /// Returns `true` if inline quick-reply is permitted on this card.
    #[must_use]
    pub fn can_reply(&self) -> bool {
        !self.content.hide_reply_button && !self.content.hide_name_and_photo && !self.unlinked
    }

    /// Starts slow hide if not already hiding or unlinked.
    pub fn start_hiding_slow(&mut self, now_ms: i64) {
        if self.unlinked || self.replying {
            return;
        }
        if self.hiding_started_at_ms.is_none() {
            self.hiding_started_at_ms = Some(now_ms);
            self.hiding_duration_ms = NOTIFY_SLOW_HIDE_MS;
        }
    }

    /// Unlinks the card and begins fast fade-out (`unlinkHistory` + `hideFast`).
    pub fn unlink_and_hide_fast(&mut self, now_ms: i64) -> bool {
        if self.unlinked {
            return false;
        }
        self.unlinked = true;
        self.replying = false;
        self.hiding_started_at_ms = Some(now_ms);
        self.hiding_duration_ms = NOTIFY_FAST_ANIM_MS;
        true
    }

    /// Pauses any active slow fade-out (`stopHiding`).
    pub fn stop_hiding(&mut self) {
        if !self.unlinked {
            self.hiding_started_at_ms = None;
            self.opacity = 1.0;
        }
    }
}

/// Computed screen rectangle for a custom popup or the "Hide all" button.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PopupGeometry {
    /// Left X coordinate in logical pixels.
    pub x: f32,
    /// Top Y coordinate in logical pixels.
    pub y: f32,
    /// Card width.
    pub width: f32,
    /// Card height.
    pub height: f32,
}

impl PopupGeometry {
    /// Converts to an `egui::Rect`.
    #[must_use]
    pub fn to_rect(self) -> Rect {
        Rect::from_min_size(
            Pos2::new(self.x, self.y),
            Vec2::new(self.width, self.height),
        )
    }
}

/// Manages stacked in-app notification popups, overflow queue, and the "Hide all" button
/// (`Window::Notifications::Default::Manager`).
#[derive(Debug)]
pub struct CustomPopupManager {
    visible: Vec<PopupCard>,
    queued: VecDeque<CustomPopupContent>,
    max_count: usize,
    corner: ScreenCorner,
    hide_all_visible: bool,
    hide_all_shift_y: f32,
    any_hovered: bool,
    demo_hidden: bool,
}

impl Default for CustomPopupManager {
    fn default() -> Self {
        Self::new(3, ScreenCorner::BottomRight)
    }
}

impl CustomPopupManager {
    /// Creates a new custom popup manager with the given `max_count` (`1..=5`) and `corner`.
    #[must_use]
    pub fn new(max_count: usize, corner: ScreenCorner) -> Self {
        Self {
            visible: Vec::new(),
            queued: VecDeque::new(),
            max_count: max_count.clamp(1, 5),
            corner,
            hide_all_visible: false,
            hide_all_shift_y: NOTIFY_DELTA_Y,
            any_hovered: false,
            demo_hidden: false,
        }
    }

    /// Synchronizes `max_count` and `corner` from [`NotificationPreferences`].
    pub fn apply_preferences(&mut self, prefs: &NotificationPreferences, now_ms: i64) {
        self.set_corner(prefs.notifications_corner);
        self.set_max_count(prefs.clamped_notifications_count(), now_ms);
    }

    /// Updates the screen corner (`ChangeType::Corner`).
    pub fn set_corner(&mut self, corner: ScreenCorner) {
        self.corner = corner;
    }

    /// Returns the configured screen corner.
    #[must_use]
    pub const fn corner(&self) -> ScreenCorner {
        self.corner
    }

    /// Updates maximum simultaneous popups (`ChangeType::MaxCount`).
    pub fn set_max_count(&mut self, max_count: usize, now_ms: i64) {
        self.max_count = max_count.clamp(1, 5);
        let mut remaining = self.max_count as isize;
        // Keep the newest `max_count` active cards, unlink older overflow cards.
        for card in self.visible.iter_mut().rev() {
            if card.unlinked {
                continue;
            }
            remaining -= 1;
            if remaining < 0 {
                card.unlink_and_hide_fast(now_ms);
            }
        }
        if remaining > 0 {
            self.promote_from_queue(now_ms, false);
        }
        self.recompute_shifts();
    }

    /// Toggles demo opacity suppression (`ChangeType::DemoIsShown` / `DemoIsHidden`).
    pub fn set_demo_shown(&mut self, shown: bool) {
        self.demo_hidden = shown;
    }

    /// Enqueues a new notification popup and promotes it if a slot is available.
    pub fn push(&mut self, content: CustomPopupContent, now_ms: i64, wait_for_input: bool) {
        self.queued.push_back(content);
        self.promote_from_queue(now_ms, wait_for_input);
    }

    /// Returns the currently visible popup cards (oldest to newest).
    #[must_use]
    pub fn visible_cards(&self) -> &[PopupCard] {
        &self.visible
    }

    /// Returns the number of active (non-unlinked) visible cards.
    #[must_use]
    pub fn active_visible_count(&self) -> usize {
        self.visible.iter().filter(|c| !c.unlinked).count()
    }

    /// Returns the number of overflow notifications waiting in the queue.
    #[must_use]
    pub fn queued_count(&self) -> usize {
        self.queued.len()
    }

    /// Returns `true` if the "Hide all" bar should be displayed (`count > 1 || !queued.is_empty()`).
    #[must_use]
    pub const fn is_hide_all_visible(&self) -> bool {
        self.hide_all_visible
    }

    /// Returns `true` if any visible notification has an open inline reply composer.
    #[must_use]
    pub fn has_replying(&self) -> bool {
        self.visible.iter().any(|c| c.replying && !c.unlinked)
    }

    /// Expands the inline reply field on the card matching `id` and pauses auto-hide.
    pub fn open_reply_for(&mut self, id: NotificationId) -> bool {
        let mut opened = false;
        for card in &mut self.visible {
            if card.content.id == id && card.can_reply() {
                card.replying = true;
                opened = true;
            }
        }
        if opened {
            self.stop_all_hiding();
            self.recompute_shifts();
        }
        opened
    }

    /// Pauses hiding on all active cards (`stopAllHiding`).
    pub fn stop_all_hiding(&mut self) {
        for card in &mut self.visible {
            card.stop_hiding();
        }
    }

    /// Resumes slow hiding on all active cards if none are replying (`startAllHiding`).
    pub fn start_all_hiding(&mut self, now_ms: i64) {
        if self.has_replying() {
            return;
        }
        for card in &mut self.visible {
            card.start_hiding_slow(now_ms);
        }
    }

    /// Advances auto-hide timers, animations, and removes finished cards.
    pub fn tick(&mut self, now_ms: i64, last_user_input_ms: Option<i64>, wait_for_input: bool) {
        let replying = self.has_replying();
        for card in &mut self.visible {
            if !card.unlinked {
                if card.wait_started_at_ms.is_none() {
                    let should_wait = wait_for_input
                        && last_user_input_ms.is_some_and(|t| t <= card.started_at_ms);
                    if !should_wait {
                        card.wait_started_at_ms = Some(now_ms);
                    }
                }
                if !self.any_hovered
                    && !replying
                    && card.hiding_started_at_ms.is_none()
                    && let Some(wait_since) = card.wait_started_at_ms
                    && now_ms.saturating_sub(wait_since) >= NOTIFY_WAIT_LONG_HIDE_MS
                {
                    card.start_hiding_slow(now_ms);
                }
            }

            if let Some(hide_start) = card.hiding_started_at_ms {
                let elapsed = now_ms.saturating_sub(hide_start);
                let dur = card.hiding_duration_ms.max(1);
                let frac = (elapsed as f32 / dur as f32).clamp(0.0, 1.0);
                card.opacity = 1.0 - frac;
            } else {
                let elapsed = now_ms.saturating_sub(card.started_at_ms);
                card.opacity = (elapsed as f32 / NOTIFY_FAST_ANIM_MS as f32).clamp(0.25, 1.0);
            }
        }

        let before = self.visible.len();
        self.visible.retain(|card| {
            let finished = card
                .hiding_started_at_ms
                .is_some_and(|start| now_ms.saturating_sub(start) >= card.hiding_duration_ms);
            !finished
        });
        if self.visible.len() != before {
            self.promote_from_queue(now_ms, wait_for_input);
        }
        self.recompute_shifts();
    }

    /// Dismisses a single card by [`NotificationId`].
    pub fn clear_item(&mut self, id: NotificationId, now_ms: i64) {
        self.queued.retain(|q| q.id != id);
        let mut changed = false;
        for card in &mut self.visible {
            if card.content.id == id && card.unlink_and_hide_fast(now_ms) {
                changed = true;
            }
        }
        if changed {
            self.promote_from_queue(now_ms, false);
            self.recompute_shifts();
        }
    }

    /// Clears all popups belonging to a forum topic.
    pub fn clear_topic(&mut self, session_id: u64, peer_id: i64, topic_root_id: i32, now_ms: i64) {
        let matches = |ctx: ContextId| {
            ctx.session_id == session_id
                && ctx.peer_id == peer_id
                && ctx.topic_root_id == topic_root_id
        };
        self.clear_matching(matches, now_ms);
    }

    /// Clears all popups belonging to a chat across all topics.
    pub fn clear_chat(&mut self, session_id: u64, peer_id: i64, now_ms: i64) {
        let matches = |ctx: ContextId| ctx.session_id == session_id && ctx.peer_id == peer_id;
        self.clear_matching(matches, now_ms);
    }

    /// Clears all popups belonging to a session.
    pub fn clear_session(&mut self, session_id: u64, now_ms: i64) {
        let matches = |ctx: ContextId| ctx.session_id == session_id;
        self.clear_matching(matches, now_ms);
    }

    /// Smoothly unlinks and dismisses all popups (`doClearAll`).
    pub fn clear_all(&mut self, now_ms: i64) {
        self.queued.clear();
        for card in &mut self.visible {
            card.unlink_and_hide_fast(now_ms);
        }
        self.recompute_shifts();
    }

    /// Immediately drops all visible and queued popups (`doClearAllFast`).
    pub fn clear_all_fast(&mut self) {
        self.queued.clear();
        self.visible.clear();
        self.hide_all_visible = false;
    }

    /// Computes screen-space rectangles for each visible card and the optional "Hide all" bar
    /// inside `bounds` (`notificationStartPosition` + `Widget::computePosition`).
    #[must_use]
    pub fn compute_layout(&self, bounds: Rect) -> (Vec<PopupGeometry>, Option<PopupGeometry>) {
        let direction = StackDirection::for_corner(self.corner);
        let x = if self.corner.is_left() {
            bounds.left() + NOTIFY_DELTA_X
        } else {
            (bounds.right() - NOTIFY_WIDTH - NOTIFY_DELTA_X).max(bounds.left() + NOTIFY_DELTA_X)
        };
        let anchor_y = if self.corner.is_top() {
            bounds.top()
        } else {
            bounds.bottom()
        };

        let compute_y = |shift: f32, height: f32| match direction {
            StackDirection::Down => anchor_y + shift,
            StackDirection::Up => anchor_y - shift - height,
        };

        let cards = self
            .visible
            .iter()
            .map(|card| {
                let h = card.height();
                PopupGeometry {
                    x,
                    y: compute_y(card.shift_y, h),
                    width: NOTIFY_WIDTH,
                    height: h,
                }
            })
            .collect();

        let hide_all = if self.hide_all_visible {
            Some(PopupGeometry {
                x,
                y: compute_y(self.hide_all_shift_y, NOTIFY_HIDE_ALL_HEIGHT),
                width: NOTIFY_WIDTH,
                height: NOTIFY_HIDE_ALL_HEIGHT,
            })
        } else {
            None
        };

        (cards, hide_all)
    }

    /// Renders all visible custom notification popups inside `ctx` and returns emitted actions.
    pub fn show_egui(
        &mut self,
        ctx: &egui::Context,
        palette: &Palette,
        now_ms: i64,
    ) -> Vec<PopupAction> {
        let mut actions = Vec::new();
        if self.demo_hidden || (self.visible.is_empty() && !self.hide_all_visible) {
            return actions;
        }

        ensure_tdesktop_fonts(ctx);
        let theme = notify_theme_palette(palette);
        let notif_bg = theme.color("notificationBg");
        let notif_border = theme.color("shadowFg");
        let bold_fg = theme.color("windowBoldFg");
        let body_fg = theme.color("windowFg");
        let accent_fg = theme.color("windowActiveTextFg");
        let active_bg = theme.color("windowBgActive");
        let close_fg = theme.color("notifyCloseIconFg");

        let bounds = ctx.content_rect();
        let (card_geoms, hide_all_geom) = self.compute_layout(bounds);

        let mut hovered_this_frame = false;
        let mut dismiss_ids = Vec::new();
        let mut submit_replies = Vec::new();
        let mut open_reply_ids = Vec::new();

        for (idx, geom) in card_geoms.into_iter().enumerate() {
            let Some(card) = self.visible.get_mut(idx) else {
                continue;
            };
            let card_id = card.content.id;
            let alpha = (card.opacity * 255.0).round().clamp(0.0, 255.0) as u8;
            let bg =
                Color32::from_rgba_unmultiplied(notif_bg.r(), notif_bg.g(), notif_bg.b(), alpha);
            let border = Color32::from_rgba_unmultiplied(
                notif_border.r(),
                notif_border.g(),
                notif_border.b(),
                alpha,
            );

            let area_id = egui::Id::unique((
                "tg_custom_notify_card",
                card_id.context.session_id,
                card_id.context.peer_id,
                card_id.msg_id,
                idx,
            ));

            let mut inner_handled = false;
            let area_resp = egui::Area::new(area_id)
                .order(Order::Foreground)
                .fixed_pos(Pos2::new(geom.x, geom.y))
                .movable(false)
                .sense(Sense::click())
                .show(ctx, |ui| {
                    // Notification text is non-selectable so clicking anywhere on the
                    // title/subtitle/body activates the card Area instead of selecting text.
                    ui.style_mut().interaction.selectable_labels = false;
                    ui.set_min_size(Vec2::new(geom.width, geom.height));
                    ui.set_max_size(Vec2::new(geom.width, geom.height));

                    Frame::NONE
                        .fill(bg)
                        .stroke(Stroke::new(1.0, border))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::same(9))
                        .show(ui, |ui| {
                            ui.set_min_width(geom.width - 18.0);
                            ui.horizontal(|ui| {
                                // Avatar circle
                                let (avatar_rect, _) = ui.allocate_exact_size(
                                    Vec2::splat(NOTIFY_PHOTO_SIZE),
                                    Sense::hover(),
                                );
                                let avatar_color = if card.content.hide_name_and_photo {
                                    active_bg
                                } else {
                                    Color32::from_rgb(
                                        card.content.avatar_rgb[0],
                                        card.content.avatar_rgb[1],
                                        card.content.avatar_rgb[2],
                                    )
                                };
                                ui.painter().circle_filled(
                                    avatar_rect.center(),
                                    NOTIFY_PHOTO_SIZE * 0.5,
                                    avatar_color,
                                );
                                let avatar_text = if card.content.hide_name_and_photo {
                                    strings::APP_MONOGRAM
                                } else {
                                    card.content.initials.as_str()
                                };
                                ui.painter().text(
                                    avatar_rect.center(),
                                    Align2::CENTER_CENTER,
                                    avatar_text,
                                    egui::FontId::proportional(20.0),
                                    Color32::WHITE,
                                );

                                ui.add_space(6.0);

                                ui.vertical(|ui| {
                                    ui.set_min_width(geom.width - NOTIFY_PHOTO_SIZE - 36.0);
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new(&card.content.title)
                                                .font(font_id_from_style(st::semibold_font))
                                                .color(bold_fg),
                                        );
                                        ui.with_layout(
                                            Layout::right_to_left(Align::Center),
                                            |ui| {
                                                let (close_rect, close_resp) = ui
                                                    .allocate_exact_size(
                                                        Vec2::splat(20.0),
                                                        Sense::click(),
                                                    );
                                                close_resp.widget_info(|| {
                                                    egui::WidgetInfo::labeled(
                                                        egui::Role::Button,
                                                        true,
                                                        "Close",
                                                    )
                                                });
                                                if close_resp.hovered() {
                                                    ui.ctx().set_cursor_icon(
                                                        egui::CursorIcon::PointingHand,
                                                    );
                                                }
                                                paint_style_icon(
                                                    ui.painter(),
                                                    close_rect,
                                                    st::notify_close.icon,
                                                    theme,
                                                    Some(close_fg),
                                                );
                                                if close_resp.clicked() {
                                                    dismiss_ids.push(card_id);
                                                    inner_handled = true;
                                                }
                                            },
                                        );
                                    });

                                    if let Some(sub) = &card.content.subtitle
                                        && !sub.is_empty()
                                    {
                                        ui.label(
                                            egui::RichText::new(sub)
                                                .font(font_id_from_style(st::dialogs_date_font))
                                                .color(accent_fg),
                                        );
                                    }

                                    ui.label(
                                        egui::RichText::new(&card.content.message)
                                            .font(font_id_from_style(st::normal_font))
                                            .color(body_fg),
                                    );

                                    if !card.replying && !card.unlinked {
                                        ui.horizontal(|ui| {
                                            if card.can_reply()
                                                && ui
                                                    .small_button(
                                                        tg_lang::tr::lng_notification_reply(),
                                                    )
                                                    .clicked()
                                            {
                                                open_reply_ids.push(card_id);
                                                inner_handled = true;
                                            }
                                            if !card.content.hide_mark_as_read
                                                && ui
                                                    .small_button(
                                                        tg_lang::tr::lng_context_mark_read(),
                                                    )
                                                    .clicked()
                                            {
                                                actions.push(PopupAction::MarkAsRead(card_id));
                                                dismiss_ids.push(card_id);
                                                inner_handled = true;
                                            }
                                        });
                                    }
                                });
                            });

                            if card.replying && !card.unlinked {
                                ui.add_space(4.0);
                                ui.horizontal(|ui| {
                                    let edit = ui.add(
                                        egui::TextEdit::singleline(&mut card.reply_draft)
                                            .hint_text(tg_lang::tr::lng_message_ph())
                                            .desired_width(geom.width - 84.0),
                                    );
                                    let enter_submit = edit.lost_focus()
                                        && ui.input(|i| i.key_pressed(egui::Key::Enter));
                                    if (ui.button(tg_lang::tr::lng_send_button()).clicked()
                                        || enter_submit)
                                        && !card.reply_draft.trim().is_empty()
                                    {
                                        submit_replies
                                            .push((card_id, card.reply_draft.trim().to_owned()));
                                        card.reply_draft.clear();
                                        card.replying = false;
                                        inner_handled = true;
                                    }
                                });
                            }
                        });
                });

            let rect = area_resp.response.rect;
            let pointer_in = ctx.input(|i| i.pointer.hover_pos().is_some_and(|p| rect.contains(p)));
            card.hovered = pointer_in;
            if pointer_in {
                hovered_this_frame = true;
            }
            if area_resp.response.hovered() && !card.replying && !card.unlinked {
                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if area_resp.response.secondary_clicked() {
                dismiss_ids.push(card_id);
            } else if !inner_handled
                && area_resp.response.clicked()
                && !card.replying
                && !card.unlinked
            {
                let draft = if card.reply_draft.trim().is_empty() {
                    None
                } else {
                    Some(card.reply_draft.clone())
                };
                actions.push(PopupAction::Activate { id: card_id, draft });
                dismiss_ids.push(card_id);
            }
        }

        if let Some(geom) = hide_all_geom {
            egui::Area::new(egui::Id::unique("tg_custom_notify_hide_all"))
                .order(Order::Foreground)
                .fixed_pos(Pos2::new(geom.x, geom.y))
                .show(ctx, |ui| {
                    let btn = egui::Button::new(
                        egui::RichText::new(tg_lang::tr::lng_notification_hide_all())
                            .font(font_id_from_style(st::semibold_font))
                            .color(accent_fg),
                    )
                    .fill(notif_bg)
                    .stroke(Stroke::new(1.0, notif_border))
                    .min_size(Vec2::new(geom.width, geom.height));
                    let resp = ui.add(btn);
                    if resp.hovered() {
                        hovered_this_frame = true;
                    }
                    if resp.clicked() {
                        actions.push(PopupAction::HideAll);
                    }
                });
        }

        if hovered_this_frame != self.any_hovered {
            self.any_hovered = hovered_this_frame;
            if hovered_this_frame {
                self.stop_all_hiding();
            } else {
                self.start_all_hiding(now_ms);
            }
        }

        for id in open_reply_ids {
            self.open_reply_for(id);
        }
        for (id, text) in submit_replies {
            actions.push(PopupAction::Reply { id, text });
            self.clear_item(id, now_ms);
        }
        for id in dismiss_ids {
            actions.push(PopupAction::Dismiss(id));
            self.clear_item(id, now_ms);
        }
        if actions.iter().any(|a| matches!(a, PopupAction::HideAll)) {
            self.clear_all(now_ms);
        }

        actions
    }

    fn clear_matching<F>(&mut self, predicate: F, now_ms: i64)
    where
        F: Fn(ContextId) -> bool,
    {
        self.queued.retain(|q| !predicate(q.id.context));
        let mut changed = false;
        for card in &mut self.visible {
            if predicate(card.content.id.context) && card.unlink_and_hide_fast(now_ms) {
                changed = true;
            }
        }
        if changed {
            self.promote_from_queue(now_ms, false);
            self.recompute_shifts();
        }
    }

    fn promote_from_queue(&mut self, now_ms: i64, wait_for_input: bool) {
        let mut free_slots = self.max_count.saturating_sub(self.active_visible_count());
        while free_slots > 0 {
            let Some(next) = self.queued.pop_front() else {
                break;
            };
            self.visible
                .push(PopupCard::new(next, now_ms, wait_for_input));
            free_slots -= 1;
        }
        self.recompute_shifts();
    }

    fn recompute_shifts(&mut self) {
        let mut shift = NOTIFY_DELTA_Y;
        let mut active_count = 0usize;
        // Newest active card sits closest to the screen edge (`moveWidgets` iterates in reverse).
        for card in self.visible.iter_mut().rev() {
            if card.unlinked {
                continue;
            }
            card.shift_y = shift;
            shift += card.height() + NOTIFY_DELTA_Y;
            active_count += 1;
        }
        self.hide_all_visible = active_count > 1 || !self.queued.is_empty();
        self.hide_all_shift_y = shift;
    }
}
