//! Self-Destruction TTL modal (`SelfDestructionBox`), `api_self_destruct` backend contract,
//! self-destructing / view-once media TTL picker, and view-once media viewer (`TtlMediaLayerWidget`).
//!
//! Port origin:
//! - `Telegram/SourceFiles/boxes/self_destruction_box.cpp`
//! - `Telegram/SourceFiles/api/api_self_destruct.cpp`
//! - `Telegram/SourceFiles/chat_helpers/ttl_media_layer_widget.cpp`
//! - `Telegram/SourceFiles/history/view/media/history_view_ephemeral_plate.cpp`

use egui::{
    Align2, Color32, CornerRadius, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2,
};
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::Colors;

trait SelfDestructColorsExt {
    fn window_bold_fg(&self) -> Color32;
    fn window_sub_text_fg(&self) -> Color32;
    fn attention_fg(&self) -> Color32;
}

impl<T: Colors + ?Sized> SelfDestructColorsExt for T {
    fn window_bold_fg(&self) -> Color32 {
        self.color("windowBoldFg")
    }
    fn window_sub_text_fg(&self) -> Color32 {
        self.color("windowSubTextFg")
    }
    fn attention_fg(&self) -> Color32 {
        self.color("attentionButtonFg")
    }
}

use crate::boxes::compose_ai::{paint_accent_button, show_modal_card};
use tg_app_core::stand_in::types::{MsgId, PeerId};

// ---------------------------------------------------------------------------
// Constants (matching `self_destruction_box.cpp` & `data_media_types.cpp`)
// ---------------------------------------------------------------------------

pub use tg_app_core::core_types::dto::self_destruct::*;

/// Operating mode of [`SelfDestructionBoxState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelfDestructionBoxType {
    /// Account inactivity self-destruction (`account.setAccountTTL`).
    Account,
    /// Inactive session auto-termination (`account.setAuthorizationTTL`).
    Sessions,
    /// Global default auto-delete timer for new chats (`messages.setDefaultHistoryTTL`).
    DefaultHistory,
}

/// Action emitted by [`SelfDestructionBoxState::show`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelfDestructionBoxAction {
    /// Saved selected TTL value (days for Account/Sessions, seconds for DefaultHistory).
    Saved {
        /// Box mode.
        kind: SelfDestructionBoxType,
        /// Chosen TTL value.
        value: i32,
    },
    /// User cancelled.
    Cancelled,
}

/// Formats days label matching `SelfDestructionBox::DaysLabel` in `self_destruction_box.cpp:186-194`.
#[must_use]
pub fn self_destruct_days_label(days: i32) -> String {
    if days <= 0 {
        return String::new();
    }
    if days > 25 {
        let months = (days / 30).max(1);
        if months == 1 {
            "1 month".to_owned()
        } else {
            format!("{months} months")
        }
    } else {
        let weeks = (days / 7).max(1);
        if weeks == 1 {
            "1 week".to_owned()
        } else {
            format!("{weeks} weeks")
        }
    }
}

/// Formats default-history TTL seconds label.
#[must_use]
pub fn default_history_ttl_label(secs: i32) -> String {
    match secs {
        0 => "Off".to_owned(),
        86_400 => "After 1 day".to_owned(),
        604_800 => "After 1 week".to_owned(),
        2_678_400 => "After 1 month".to_owned(),
        s if s % 86_400 == 0 => format!("After {} days", s / 86_400),
        s => format!("After {s} seconds"),
    }
}

/// Picks the closest canonical option in `options` to `current` (`SelfDestructionBox::gotCurrent`).
#[must_use]
pub fn nearest_ttl_option(current: i32, options: &[i32]) -> i32 {
    let mut best = options.first().copied().unwrap_or(current);
    for &val in options {
        if (current - val).abs() < (current - best).abs() {
            best = val;
        }
    }
    best
}

/// Stateful Self-Destruction TTL modal (`SelfDestructionBox`).
#[derive(Debug, Clone)]
pub struct SelfDestructionBoxState {
    /// Mode (Account, Sessions, or DefaultHistory).
    pub kind: SelfDestructionBoxType,
    /// Available canonical option values.
    pub options: Vec<i32>,
    /// Currently selected option value.
    pub selected: i32,
    /// Whether initial value has been loaded from backend.
    pub loaded: bool,
    /// Inline RPC error, if any.
    pub error: Option<String>,
}

impl SelfDestructionBoxState {
    /// Creates a `SelfDestructionBox` for `kind` with initial `current_value`.
    #[must_use]
    pub fn new(kind: SelfDestructionBoxType, current_value: i32) -> Self {
        let options: Vec<i32> = match kind {
            SelfDestructionBoxType::Account => ACCOUNT_TTL_DAYS_OPTIONS.to_vec(),
            SelfDestructionBoxType::Sessions => SESSIONS_TTL_DAYS_OPTIONS.to_vec(),
            SelfDestructionBoxType::DefaultHistory => DEFAULT_HISTORY_TTL_SECS_OPTIONS.to_vec(),
        };
        let selected = nearest_ttl_option(current_value, &options);
        Self {
            kind,
            options,
            selected,
            loaded: false,
            error: None,
        }
    }

    /// Loads current server TTL from `backend` if not loaded yet.
    pub fn ensure_loaded(&mut self, backend: &mut dyn SelfDestructBackend) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        if let Ok(snap) = backend.reload_self_destruct() {
            let current = match self.kind {
                SelfDestructionBoxType::Account => snap.account_ttl_days,
                SelfDestructionBoxType::Sessions => snap.sessions_ttl_days,
                SelfDestructionBoxType::DefaultHistory => snap.default_history_ttl_secs,
            };
            self.selected = nearest_ttl_option(current, &self.options);
        }
    }

    /// Saves the selected TTL value via `backend`.
    pub fn save(&mut self, backend: &mut dyn SelfDestructBackend) -> Result<i32, String> {
        let res = match self.kind {
            SelfDestructionBoxType::Account => backend.update_account_ttl(self.selected),
            SelfDestructionBoxType::Sessions => backend.update_sessions_ttl(self.selected),
            SelfDestructionBoxType::DefaultHistory => {
                backend.update_default_history_ttl(self.selected)
            }
        };
        match res {
            Ok(()) => {
                self.error = None;
                Ok(self.selected)
            }
            Err(err) => {
                self.error = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Builds a `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        let title = match self.kind {
            SelfDestructionBoxType::Account => "If Away For",
            SelfDestructionBoxType::Sessions => "Terminate Inactive Sessions",
            SelfDestructionBoxType::DefaultHistory => "Auto-Delete Messages",
        };
        BoxModal::new(id, title)
            .width(360.0)
            .add_button(BoxButton::default_btn("save", "Save"))
            .add_button(BoxButton::default_btn("cancel", "Cancel"))
    }

    /// Renders the `SelfDestructionBox` modal.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn SelfDestructBackend,
        colors: &dyn Colors,
    ) -> Option<SelfDestructionBoxAction> {
        self.ensure_loaded(backend);
        let mut action = None;

        show_modal_card(ui, "tg_self_destruct_modal", 360.0, colors, |ui| {
            let (title, description) = match self.kind {
                SelfDestructionBoxType::Account => (
                    "If Away For",
                    "If you do not come online at least once within this period, your account will be deleted along with all messages and contacts.",
                ),
                SelfDestructionBoxType::Sessions => (
                    "Terminate Old Sessions",
                    "Automatically terminate sessions that have been inactive for the selected period.",
                ),
                SelfDestructionBoxType::DefaultHistory => (
                    "Auto-Delete Messages",
                    "Automatically delete new messages sent in newly started chats after this period.",
                ),
            };

            ui.label(
                egui::RichText::new(title)
                    .strong()
                    .size(16.0)
                    .color(colors.window_bold_fg()),
            );
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(description)
                    .size(12.5)
                    .color(colors.window_sub_text_fg()),
            );
            ui.add_space(10.0);

            for &val in &self.options {
                let label = match self.kind {
                    SelfDestructionBoxType::Account | SelfDestructionBoxType::Sessions => {
                        self_destruct_days_label(val)
                    }
                    SelfDestructionBoxType::DefaultHistory => default_history_ttl_label(val),
                };
                if ui.radio(self.selected == val, label).clicked() {
                    self.selected = val;
                    self.error = None;
                }
            }

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
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if paint_accent_button(ui, "Save", colors)
                        && let Ok(value) = self.save(backend)
                    {
                        action = Some(SelfDestructionBoxAction::Saved {
                            kind: self.kind,
                            value,
                        });
                    }
                    ui.add_space(8.0);
                    if ui.button("Cancel").clicked() {
                        action = Some(SelfDestructionBoxAction::Cancelled);
                    }
                });
            });
        });

        if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
            action = Some(SelfDestructionBoxAction::Cancelled);
        }
        action
    }
}

// ---------------------------------------------------------------------------
// 2. View-Once / TTL Media Layer Viewer (`ttl_media_layer_widget.cpp`)
// ---------------------------------------------------------------------------

/// Media kind displayed inside [`ViewOnceMediaModalState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewOnceMediaKind {
    /// Self-destructing photo.
    Photo,
    /// Self-destructing video.
    Video,
    /// View-once voice message.
    Voice,
    /// View-once circular video note.
    RoundVideo,
}

impl ViewOnceMediaKind {
    /// Friendly display label for the view-once plate & viewer header.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Photo => "Photo",
            Self::Video => "Video",
            Self::Voice => "Voice Message",
            Self::RoundVideo => "Video Message",
        }
    }
}

/// Outcome emitted when closing or expiring a [`ViewOnceMediaModalState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewOnceModalAction {
    /// Media finished viewing and should be marked expired/burned (`messages.readMessageContents`).
    Consumed {
        /// Chat peer ID.
        peer_id: PeerId,
        /// Message ID.
        msg_id: MsgId,
    },
}

/// Fullscreen-style overlay modal for viewing a self-destructing / view-once media item
/// (`ChatHelpers::TtlMediaLayerWidget`).
#[derive(Debug, Clone)]
pub struct ViewOnceMediaModalState {
    /// Source peer ID.
    pub peer_id: PeerId,
    /// Source message ID.
    pub msg_id: MsgId,
    /// Sender display name.
    pub sender_name: String,
    /// Media kind.
    pub kind: ViewOnceMediaKind,
    /// Wire `ttl_seconds` (`VIEW_ONCE_TTL_SECONDS` or `1..=60`).
    pub ttl_seconds: i32,
    /// Optional caption or media summary.
    pub caption: String,
    /// Accent gradient RGB pair for placeholder artwork.
    pub preview_rgb: ([u8; 3], [u8; 3]),
    /// Timestamp (`ui.ctx().input(|i| i.time)`) when the viewer opened.
    pub opened_at_secs: f64,
    /// Whether `messages.readMessageContents` has already been dispatched.
    pub read_acknowledged: bool,
}

impl ViewOnceMediaModalState {
    /// Opens a view-once / TTL media viewer overlay.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        peer_id: PeerId,
        msg_id: MsgId,
        sender_name: impl Into<String>,
        kind: ViewOnceMediaKind,
        ttl_seconds: i32,
        caption: impl Into<String>,
        preview_rgb: ([u8; 3], [u8; 3]),
        now_secs: f64,
    ) -> Self {
        Self {
            peer_id,
            msg_id,
            sender_name: sender_name.into(),
            kind,
            ttl_seconds,
            caption: caption.into(),
            preview_rgb,
            opened_at_secs: now_secs,
            read_acknowledged: false,
        }
    }

    /// Remaining seconds for timed self-destruct (`None` for single-view media that closes manually).
    #[must_use]
    pub fn remaining_secs(&self, now_secs: f64) -> Option<f64> {
        if self.ttl_seconds == VIEW_ONCE_TTL_SECONDS || self.ttl_seconds <= 0 {
            return None;
        }
        let elapsed = (now_secs - self.opened_at_secs).max(0.0);
        Some((f64::from(self.ttl_seconds) - elapsed).max(0.0))
    }

    /// Renders the view-once media viewer modal; returns `Some(Consumed)` when closed or expired.
    pub fn show(&mut self, ui: &mut Ui, now_secs: f64) -> Option<ViewOnceModalAction> {
        if let Some(rem) = self.remaining_secs(now_secs) {
            if rem <= 0.0 {
                return Some(ViewOnceModalAction::Consumed {
                    peer_id: self.peer_id,
                    msg_id: self.msg_id,
                });
            }
            ui.ctx().request_repaint();
        }

        let mut close_now = false;
        let screen = ui.ctx().viewport_rect();
        egui::Area::new(egui::Id::unique(("tg_view_once_backdrop", self.msg_id.0)))
            .order(egui::Order::Foreground)
            .fixed_pos(screen.min)
            .show(ui.ctx(), |ui| {
                let (r, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
                ui.painter().rect_filled(
                    r,
                    CornerRadius::ZERO,
                    Color32::from_rgba_unmultiplied(8, 10, 14, 232),
                );
                if resp.clicked() {
                    close_now = true;
                }
            });

        egui::Area::new(egui::Id::unique(("tg_view_once_card", self.msg_id.0)))
            .order(egui::Order::Tooltip)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                egui::Frame::NONE
                    .fill(Color32::from_rgb(0x16, 0x1B, 0x22))
                    .stroke(Stroke::new(1.0, Color32::from_rgb(0x30, 0x3A, 0x48)))
                    .corner_radius(CornerRadius::same(14))
                    .inner_margin(egui::Margin::same(18))
                    .show(ui, |ui| {
                        ui.set_width(380.0);
                        ui.horizontal(|ui| {
                            let is_view_once = self.ttl_seconds == VIEW_ONCE_TTL_SECONDS;
                            let badge = if is_view_once {
                                "1 · View Once".to_owned()
                            } else {
                                let rem = self
                                    .remaining_secs(now_secs)
                                    .unwrap_or(0.0)
                                    .ceil() as i32;
                                format!("{rem}s remaining")
                            };
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} — {}",
                                    self.kind.display_name(),
                                    self.sender_name
                                ))
                                .strong()
                                .size(14.0)
                                .color(Color32::WHITE),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let badge_col = Color32::from_rgb(0x6A, 0xC1, 0xFF);
                                    ui.label(
                                        egui::RichText::new(badge)
                                            .strong()
                                            .size(12.0)
                                            .color(badge_col),
                                    );
                                    if !is_view_once {
                                        let (r, _) =
                                            ui.allocate_exact_size(Vec2::splat(13.0), Sense::hover());
                                        tg_app_core::ui_icons::paint_style_tinted(
                                            ui.painter(),
                                            r,
                                            tg_ui::tg_style::st::menu_icon_ttl,
                                            badge_col,
                                        );
                                    }
                                },
                            );
                        });
                        ui.add_space(10.0);

                        let (preview_rect, _) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), 240.0),
                            Sense::hover(),
                        );
                        let (c0, c1) = self.preview_rgb;
                        let bg = Color32::from_rgb(c0[0], c0[1], c0[2]);
                        let accent = Color32::from_rgb(c1[0], c1[1], c1[2]);
                        if self.kind == ViewOnceMediaKind::RoundVideo {
                            ui.painter().circle_filled(preview_rect.center(), 100.0, bg);
                            ui.painter().circle_stroke(
                                preview_rect.center(),
                                100.0,
                                Stroke::new(3.0, accent),
                            );
                        } else {
                            ui.painter()
                                .rect_filled(preview_rect, CornerRadius::same(10), bg);
                            ui.painter().rect_stroke(
                                preview_rect,
                                CornerRadius::same(10),
                                Stroke::new(1.5, accent),
                                StrokeKind::Inside,
                            );
                        }
                        ui.painter().text(
                            preview_rect.center(),
                            Align2::CENTER_CENTER,
                            match self.kind {
                                ViewOnceMediaKind::Photo => "Protected View-Once Photo",
                                ViewOnceMediaKind::Video => "Protected View-Once Video",
                                ViewOnceMediaKind::Voice => "Playing View-Once Voice Note",
                                ViewOnceMediaKind::RoundVideo => "Playing View-Once Video Note",
                            },
                            FontId::proportional(14.0),
                            Color32::WHITE,
                        );

                        if !self.caption.trim().is_empty() {
                            ui.add_space(8.0);
                            ui.label(
                                egui::RichText::new(&self.caption)
                                    .size(13.0)
                                    .color(Color32::from_rgb(0xDC, 0xE3, 0xEC)),
                            );
                        }

                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new(
                                "Screenshots and forwarding are disabled. This media disappears once closed.",
                            )
                            .size(11.0)
                            .color(Color32::from_rgb(0x96, 0xA4, 0xB5)),
                        );
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("Close & Delete").clicked() {
                                        close_now = true;
                                    }
                                },
                            );
                        });
                    });
            });

        if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
            close_now = true;
        }
        close_now.then_some(ViewOnceModalAction::Consumed {
            peer_id: self.peer_id,
            msg_id: self.msg_id,
        })
    }
}

/// Paints the view-once / self-destructing spoiler plate over a chat bubble media rect
/// (`history_view_ephemeral_plate.cpp` & `ui/controls/ttl_media.cpp`).
///
/// Returns `true` when the user clicked an unexpired view-once plate to open it.
pub fn paint_view_once_media_plate(
    ui: &mut Ui,
    msg_id: MsgId,
    rect: Rect,
    kind: ViewOnceMediaKind,
    ttl_seconds: i32,
    viewed: bool,
) -> bool {
    let bg = if viewed {
        Color32::from_rgba_unmultiplied(24, 28, 34, 210)
    } else {
        Color32::from_rgba_unmultiplied(18, 28, 42, 218)
    };
    ui.painter().rect_filled(rect, CornerRadius::same(10), bg);

    // Subtle sparkle dots for unexpired self-destructing cover
    if !viewed {
        for i in 0..14u32 {
            let fx = (((i.wrapping_mul(73) ^ (msg_id.0 as u32)) % 88) + 6) as f32 / 100.0;
            let fy = (((i.wrapping_mul(41) ^ ((msg_id.0 as u32).wrapping_mul(7))) % 84) + 8) as f32
                / 100.0;
            let p = Pos2::new(
                rect.left() + rect.width() * fx,
                rect.top() + rect.height() * fy,
            );
            ui.painter()
                .circle_filled(p, 1.6, Color32::from_rgba_unmultiplied(255, 255, 255, 95));
        }
    }

    // Center badge circle ("1" for view-once, flame/timer for timed, lock/check for expired)
    let center = rect.center();
    let badge_r = 22.0f32.min(rect.height() * 0.28);
    ui.painter().circle_filled(
        center,
        badge_r,
        if viewed {
            Color32::from_rgba_unmultiplied(90, 98, 110, 180)
        } else {
            Color32::from_rgba_unmultiplied(56, 158, 232, 225)
        },
    );
    if viewed {
        tg_app_core::ui_icons::paint_check(
            ui.painter(),
            center,
            (badge_r * 1.0).max(13.0),
            Color32::WHITE,
        );
    } else if ttl_seconds == VIEW_ONCE_TTL_SECONDS {
        ui.painter().text(
            center,
            Align2::CENTER_CENTER,
            "1",
            FontId::proportional((badge_r * 0.9).max(12.0)),
            Color32::WHITE,
        );
    } else {
        tg_app_core::ui_icons::paint_style_tinted(
            ui.painter(),
            Rect::from_center_size(center, Vec2::splat((badge_r * 1.15).max(16.0))),
            tg_ui::tg_style::st::menu_icon_ttl,
            Color32::WHITE,
        );
    }

    let subtitle = if viewed {
        format!("{} Expired", kind.display_name())
    } else if ttl_seconds == VIEW_ONCE_TTL_SECONDS {
        format!("View Once {}", kind.display_name())
    } else {
        format!("{} · {ttl_seconds}s", kind.display_name())
    };
    ui.painter().text(
        Pos2::new(
            center.x,
            (center.y + badge_r + 12.0).min(rect.bottom() - 10.0),
        ),
        Align2::CENTER_CENTER,
        subtitle,
        FontId::proportional(11.5),
        Color32::WHITE,
    );

    let resp = ui.interact(
        rect,
        egui::Id::unique(("tg_view_once_plate", msg_id.0)),
        Sense::click(),
    );
    if resp.hovered() && !viewed {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    !viewed && resp.clicked()
}
