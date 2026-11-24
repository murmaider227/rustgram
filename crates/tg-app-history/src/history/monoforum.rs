//! Suggested posts and Channel Direct Messages (monoforum): price/time chooser, commission
//! math, suggest-options compose bar, suggest-decision bubble card, monoforum sublist model,
//! and `MonoforumSectionWidget`.
//!
//! Port origin:
//! - `Telegram/SourceFiles/history/view/controls/history_view_suggest_options.cpp`
//! - `Telegram/SourceFiles/history/view/media/history_view_suggest_decision.cpp`
//! - `Telegram/SourceFiles/history/view/history_view_chat_section_monoforum.cpp`
//! - `Telegram/SourceFiles/api/api_suggest_post.cpp`
//! - `Telegram/SourceFiles/data/data_saved_sublist.cpp` (monoforum parts)

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Pos2, Rect, ScrollArea, Sense, Stroke,
    TextEdit, Ui, Vec2,
};
use tg_text::TextWithEntities;

use crate::history::backend::{HistoryMessage, Layer233ExtrasBackend};
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::types::{MsgId, PeerId};

pub use tg_app_core::core_types::dto::monoforum::*;

/// User action emitted by [`paint_suggested_post_card`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuggestCardAction {
    /// Approve this suggested post (`Api::AcceptClickHandler`).
    Accept(MsgId),
    /// Open the decline confirmation dialog (`Api::DeclineClickHandler`).
    Decline(MsgId),
    /// Propose price/time changes (`Api::SuggestChangesClickHandler`).
    SuggestChanges(MsgId),
}

/// Height reserved for a suggested-post decision card.
pub const SUGGEST_CARD_HEIGHT: f32 = 96.0;

/// Paints an interactive suggested-post offer card with Accept / Decline / Suggest Changes controls.
pub fn paint_suggested_post_card(
    ui: &mut Ui,
    msg_id: MsgId,
    data: &SuggestedPostData,
    is_admin: bool,
    rect: Rect,
    colors: &HistoryColors,
) -> Option<SuggestCardAction> {
    let mut action = None;
    ui.painter()
        .rect_filled(rect, CornerRadius::same(8), colors.msg_in_reply_bg);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(8),
        Stroke::new(1.0, colors.divider),
        egui::StrokeKind::Inside,
    );

    let left = rect.left() + 10.0;
    let mut y = rect.top() + 7.0;

    let status_text = match data.status() {
        SuggestedPostStatus::Pending => "Suggested Post",
        SuggestedPostStatus::Accepted => "Suggested Post • Approved",
        SuggestedPostStatus::Rejected => "Suggested Post • Declined",
    };
    let status_col = match data.status() {
        SuggestedPostStatus::Pending => colors.active_text_fg,
        SuggestedPostStatus::Accepted => Color32::from_rgb(0x2E, 0xA0, 0x43),
        SuggestedPostStatus::Rejected => colors.attention_fg,
    };
    ui.painter().text(
        Pos2::new(left, y),
        Align2::LEFT_TOP,
        status_text,
        FontId::proportional(12.0),
        status_col,
    );
    y += 18.0;

    let price_str = data
        .price
        .as_ref()
        .map_or_else(|| "Free".to_owned(), SuggestedPrice::format_label);
    let time_str = format_suggest_schedule_date(data.schedule_date);
    ui.painter().text(
        Pos2::new(left, y),
        Align2::LEFT_TOP,
        format!("Price: {price_str}"),
        FontId::proportional(12.0),
        colors.text_fg,
    );
    y += 16.0;
    ui.painter().text(
        Pos2::new(left, y),
        Align2::LEFT_TOP,
        format!("Time: {time_str}"),
        FontId::proportional(12.0),
        colors.sub_text_fg,
    );
    y += 20.0;

    if data.is_pending() {
        let btn_h = 24.0;
        let btn_gap = 6.0;
        let avail = (rect.width() - 20.0).max(150.0);
        let btn_w = ((avail - btn_gap * 2.0) / 3.0).max(46.0);

        let accept_rect = Rect::from_min_size(Pos2::new(left, y), Vec2::new(btn_w, btn_h));
        let decline_rect = Rect::from_min_size(
            Pos2::new(accept_rect.right() + btn_gap, y),
            Vec2::new(btn_w, btn_h),
        );
        let edit_rect = Rect::from_min_size(
            Pos2::new(decline_rect.right() + btn_gap, y),
            Vec2::new(btn_w, btn_h),
        );

        let accept_label = if is_admin { "Approve" } else { "Accept" };
        let accept_resp = ui.interact(
            accept_rect,
            Id::unique(("tg_suggest_accept", msg_id.0)),
            Sense::click(),
        );
        accept_resp
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, accept_label));
        if accept_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        ui.painter()
            .rect_filled(accept_rect, CornerRadius::same(6), colors.accent);
        ui.painter().text(
            accept_rect.center(),
            Align2::CENTER_CENTER,
            accept_label,
            FontId::proportional(11.5),
            Color32::WHITE,
        );
        if accept_resp.clicked() {
            action = Some(SuggestCardAction::Accept(msg_id));
        }

        let decline_resp = ui.interact(
            decline_rect,
            Id::unique(("tg_suggest_decline", msg_id.0)),
            Sense::click(),
        );
        decline_resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Decline"));
        if decline_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        ui.painter()
            .rect_filled(decline_rect, CornerRadius::same(6), colors.reaction_in_bg);
        ui.painter().text(
            decline_rect.center(),
            Align2::CENTER_CENTER,
            "Decline",
            FontId::proportional(11.5),
            colors.attention_fg,
        );
        if decline_resp.clicked() {
            action = Some(SuggestCardAction::Decline(msg_id));
        }

        let edit_resp = ui.interact(
            edit_rect,
            Id::unique(("tg_suggest_change", msg_id.0)),
            Sense::click(),
        );
        edit_resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Change"));
        if edit_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        ui.painter()
            .rect_filled(edit_rect, CornerRadius::same(6), colors.reaction_in_bg);
        ui.painter().text(
            edit_rect.center(),
            Align2::CENTER_CENTER,
            "Change",
            FontId::proportional(11.5),
            colors.text_fg,
        );
        if edit_resp.clicked() {
            action = Some(SuggestCardAction::SuggestChanges(msg_id));
        }
    }

    action
}

// ---------------------------------------------------------------------------
// SuggestOptionsBar & SuggestOptionsBoxState (`history_view_suggest_options.cpp`)
// ---------------------------------------------------------------------------

/// Action emitted by [`paint_suggest_options_bar`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestOptionsBarAction {
    /// Click bar body to edit offer price/time.
    Edit,
    /// Click close button to clear suggested post options.
    Clear,
}

/// Paints the compose-header bar showing the active suggested post offer (`SuggestOptionsBar`).
pub fn paint_suggest_options_bar(
    ui: &mut Ui,
    rect: Rect,
    options: &SuggestOptions,
    mode: SuggestMode,
    colors: &HistoryColors,
) -> Option<SuggestOptionsBarAction> {
    let mut action = None;
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, colors.bar_bg);
    ui.painter().line_segment(
        [
            Pos2::new(rect.left(), rect.top()),
            Pos2::new(rect.right(), rect.top()),
        ],
        Stroke::new(1.0, colors.divider),
    );

    let title = match mode {
        SuggestMode::New => "Suggested Post",
        SuggestMode::Change => "Suggest Changes",
        SuggestMode::Publish => "Schedule Publication",
    };
    let price_text = options
        .price
        .as_ref()
        .map_or_else(|| "Free".to_owned(), SuggestedPrice::format_label);
    let date_text = format_suggest_schedule_date(options.schedule_date);
    let subtitle = format!("{price_text}  •  {date_text}");

    let body_rect = Rect::from_min_max(
        Pos2::new(rect.left() + 12.0, rect.top() + 4.0),
        Pos2::new(rect.right() - 36.0, rect.bottom() - 4.0),
    );
    let body_resp = ui.interact(body_rect, Id::unique("tg_suggest_bar_body"), Sense::click());
    if body_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    ui.painter().text(
        Pos2::new(body_rect.left(), body_rect.top() + 2.0),
        Align2::LEFT_TOP,
        title,
        FontId::proportional(12.0),
        colors.active_text_fg,
    );
    ui.painter().text(
        Pos2::new(body_rect.left(), body_rect.top() + 18.0),
        Align2::LEFT_TOP,
        subtitle,
        FontId::proportional(11.5),
        colors.sub_text_fg,
    );
    if body_resp.clicked() {
        action = Some(SuggestOptionsBarAction::Edit);
    }

    let close_rect = Rect::from_min_size(
        Pos2::new(rect.right() - 32.0, rect.center().y - 12.0),
        Vec2::splat(24.0),
    );
    let close_resp = ui.interact(
        close_rect,
        Id::unique("tg_suggest_bar_close"),
        Sense::click(),
    );
    close_resp.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::Button, true, "Clear Suggested Post")
    });
    if close_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    tg_app_core::ui_icons::paint_close_cross(
        ui.painter(),
        close_rect.center(),
        13.0,
        colors.sub_text_fg,
    );
    if close_resp.clicked() {
        action = Some(SuggestOptionsBarAction::Clear);
    }

    action
}

/// Outcome emitted by [`SuggestOptionsBoxState::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuggestOptionsBoxAction {
    /// Confirmed valid suggested post options.
    Confirmed(SuggestOptions),
    /// User cancelled.
    Cancelled,
}

/// Modal dialog for selecting suggested post price (Stars or TON) and optional publish time
/// (`ChooseSuggestPriceBox` + `ChooseSuggestTimeBox` in `history_view_suggest_options.cpp`).
#[derive(Debug, Clone)]
pub struct SuggestOptionsBoxState {
    /// Modal operating mode (`New`, `Change`, `Publish`).
    pub mode: SuggestMode,
    /// True if opened by a channel admin (shows commission payout preview).
    pub is_admin: bool,
    /// Active currency tab (`Stars` or `Ton`).
    pub currency: SuggestedCurrency,
    /// Stars amount text input.
    pub stars_input: String,
    /// TON amount text input.
    pub ton_input: String,
    /// Whether a specific publish time is enabled (false = "Any time").
    pub use_schedule: bool,
    /// Selected delay in hours from `now_unix` when `use_schedule == true`.
    pub delay_hours: u32,
    /// Reference current Unix timestamp for clamping `[now + min, now + max]`.
    pub now_unix: i32,
    /// Optional validation error message.
    pub error: Option<String>,
}

impl SuggestOptionsBoxState {
    /// Creates a new `SuggestOptionsBoxState` seeded with `initial`.
    #[must_use]
    pub fn new(mode: SuggestMode, is_admin: bool, initial: &SuggestOptions, now_unix: i32) -> Self {
        let (currency, stars_input, ton_input) = match initial.price {
            Some(p) if p.currency == SuggestedCurrency::Ton && !p.is_free() => {
                let ton = if p.nanos == 0 {
                    p.whole.to_string()
                } else {
                    let frac = format!("{:09}", p.nanos.unsigned_abs());
                    format!("{}.{}", p.whole, frac.trim_end_matches('0'))
                };
                (SuggestedCurrency::Ton, String::new(), ton)
            }
            Some(p) if !p.is_free() => {
                (SuggestedCurrency::Stars, p.whole.to_string(), String::new())
            }
            _ => (SuggestedCurrency::Stars, String::new(), String::new()),
        };
        let (use_schedule, delay_hours) = match initial.schedule_date.filter(|&d| d > now_unix) {
            Some(ts) => (true, ((ts - now_unix) / 3600).clamp(1, 744) as u32),
            None => (false, 24),
        };
        Self {
            mode,
            is_admin,
            currency,
            stars_input,
            ton_input,
            use_schedule,
            delay_hours,
            now_unix,
            error: None,
        }
    }

    /// Computes and validates the current [`SuggestOptions`].
    pub fn validate_and_build(&self) -> Result<SuggestOptions, String> {
        let price = match self.currency {
            SuggestedCurrency::Stars => {
                let s = self.stars_input.trim();
                if s.is_empty() || s == "0" {
                    None
                } else {
                    let n: i64 = s
                        .parse()
                        .map_err(|_| "Enter a valid whole number of Stars.".to_owned())?;
                    if !(SUGGESTED_POST_STARS_MIN..=SUGGESTED_POST_STARS_MAX).contains(&n) {
                        return Err(format!(
                            "Stars offer must be between {SUGGESTED_POST_STARS_MIN} and {SUGGESTED_POST_STARS_MAX}."
                        ));
                    }
                    Some(SuggestedPrice::stars(n))
                }
            }
            SuggestedCurrency::Ton => {
                let s = self.ton_input.trim();
                if s.is_empty() || s == "0" {
                    None
                } else {
                    let nanos = SuggestedPrice::parse_ton_nanos(s)
                        .ok_or_else(|| "Enter a valid TON amount (e.g. 1.5).".to_owned())?;
                    if nanos == 0 {
                        None
                    } else if !(SUGGESTED_POST_NANOTON_MIN..=SUGGESTED_POST_NANOTON_MAX)
                        .contains(&nanos)
                    {
                        return Err("TON offer must be between 0.01 and 10,000 TON.".to_owned());
                    } else {
                        Some(SuggestedPrice::ton_nanos(nanos))
                    }
                }
            }
        };

        let schedule_date = if self.use_schedule {
            let min_ts = self
                .now_unix
                .saturating_add(SUGGESTED_POST_DELAY_MIN_SECS + 60);
            let max_ts = self.now_unix.saturating_add(SUGGESTED_POST_DELAY_MAX_SECS);
            let raw = self
                .now_unix
                .saturating_add((self.delay_hours as i32).saturating_mul(3600));
            Some(raw.clamp(min_ts, max_ts))
        } else {
            None
        };

        Ok(SuggestOptions {
            price,
            schedule_date,
        })
    }

    /// Renders `SuggestOptionsBoxState`.
    pub fn show(&mut self, ui: &mut Ui, colors: &HistoryColors) -> Option<SuggestOptionsBoxAction> {
        let mut action = None;
        let title = match self.mode {
            SuggestMode::New => "Suggest Post Offer",
            SuggestMode::Change => "Suggest Changes",
            SuggestMode::Publish => "Publish Suggested Post",
        };
        egui::Window::new(title)
            .id(Id::unique("tg_suggest_options_box"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .fixed_size(Vec2::new(360.0, 270.0))
            .show(ui.ctx(), |ui| {
                ui.horizontal(|ui| {
                    let sel_bg = ui.visuals().selection.bg_fill;
                    let star_painter = |p: &egui::Painter, c: Pos2, sz: f32, col: Color32| {
                        tg_app_core::ui_icons::paint_star(p, c, sz * 0.45, col);
                    };
                    if tg_app_core::ui_icons::selectable_icon_chip(
                        ui,
                        "tg_suggest_cur_stars",
                        self.currency == SuggestedCurrency::Stars,
                        "Telegram Stars",
                        12.5,
                        colors.text_fg,
                        sel_bg,
                        colors.bar_bg_over,
                        Color32::from_rgb(0xF5, 0xB8, 0x2E),
                        Some(&star_painter),
                    )
                    .clicked()
                    {
                        self.currency = SuggestedCurrency::Stars;
                    }
                    let ton_painter = |p: &egui::Painter, c: Pos2, sz: f32, col: Color32| {
                        tg_app_core::ui_icons::paint_diamond(p, c, sz, col);
                    };
                    if tg_app_core::ui_icons::selectable_icon_chip(
                        ui,
                        "tg_suggest_cur_ton",
                        self.currency == SuggestedCurrency::Ton,
                        "TON",
                        12.5,
                        colors.text_fg,
                        sel_bg,
                        colors.bar_bg_over,
                        colors.active_text_fg,
                        Some(&ton_painter),
                    )
                    .clicked()
                    {
                        self.currency = SuggestedCurrency::Ton;
                    }
                });
                ui.add_space(6.0);

                match self.currency {
                    SuggestedCurrency::Stars => {
                        ui.label("Stars amount (leave empty for free):");
                        ui.add(
                            TextEdit::singleline(&mut self.stars_input)
                                .id(Id::unique("tg_suggest_stars_input"))
                                .hint_text("e.g. 250")
                                .desired_width(320.0),
                        );
                    }
                    SuggestedCurrency::Ton => {
                        ui.label("TON amount (leave empty for free):");
                        ui.add(
                            TextEdit::singleline(&mut self.ton_input)
                                .id(Id::unique("tg_suggest_ton_input"))
                                .hint_text("e.g. 1.5")
                                .desired_width(320.0),
                        );
                    }
                }

                if let Ok(preview) = self.validate_and_build()
                    && let Some(price) = preview.price
                {
                    let comm = match price.currency {
                        SuggestedCurrency::Stars => SUGGESTED_POST_COMMISSION_STARS,
                        SuggestedCurrency::Ton => SUGGESTED_POST_COMMISSION_TON,
                    };
                    let payout = price.after_commission(comm);
                    let pct = SuggestedPrice::format_commission_percent(comm);
                    let note = if self.is_admin {
                        format!("Channel receives {} ({pct}).", payout.format_label())
                    } else {
                        format!("Offer: {} (creator keeps {pct}).", price.format_label())
                    };
                    ui.label(
                        egui::RichText::new(note)
                            .size(11.5)
                            .color(colors.sub_text_fg),
                    );
                }

                ui.add_space(6.0);
                ui.checkbox(&mut self.use_schedule, "Specify publish time");
                if self.use_schedule {
                    ui.horizontal(|ui| {
                        ui.label("Publish in (hours):");
                        ui.add(egui::DragValue::new(&mut self.delay_hours).range(1..=744));
                    });
                }

                if let Some(ref err) = self.error {
                    ui.label(egui::RichText::new(err).color(colors.attention_fg));
                }

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        action = Some(SuggestOptionsBoxAction::Cancelled);
                    }
                    if ui.button("Save Offer").clicked() {
                        match self.validate_and_build() {
                            Ok(opts) => {
                                self.error = None;
                                action = Some(SuggestOptionsBoxAction::Confirmed(opts));
                            }
                            Err(err) => {
                                self.error = Some(err);
                            }
                        }
                    }
                });
            });

        action
    }
}

// ---------------------------------------------------------------------------
// DeclineSuggestBoxState (`RequestDeclineComment` in `api/api_suggest_post.cpp`)
// ---------------------------------------------------------------------------

/// Action emitted by [`DeclineSuggestBoxState::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclineSuggestBoxAction {
    /// Confirm declining `msg_id` with optional rejection comment.
    ConfirmDecline {
        /// Suggested post message id.
        msg_id: MsgId,
        /// Optional rejection reason.
        comment: Option<String>,
    },
    /// User cancelled.
    Cancelled,
}

/// Modal dialog asking for an optional decline reason (`RequestDeclineComment`).
#[derive(Debug, Clone)]
pub struct DeclineSuggestBoxState {
    /// Target suggested post message id.
    pub msg_id: MsgId,
    /// Optional rejection comment text.
    pub reason: String,
}

impl DeclineSuggestBoxState {
    /// Opens a decline confirmation box for `msg_id`.
    #[must_use]
    pub const fn new(msg_id: MsgId) -> Self {
        Self {
            msg_id,
            reason: String::new(),
        }
    }

    /// Renders the decline confirmation modal.
    pub fn show(&mut self, ui: &mut Ui, colors: &HistoryColors) -> Option<DeclineSuggestBoxAction> {
        let mut action = None;
        egui::Window::new("Decline Suggested Post")
            .id(Id::unique("tg_decline_suggest_box"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .fixed_size(Vec2::new(340.0, 170.0))
            .show(ui.ctx(), |ui| {
                ui.label("Optionally add a reason for declining this suggested post:");
                ui.add(
                    TextEdit::singleline(&mut self.reason)
                        .id(Id::unique("tg_decline_suggest_reason"))
                        .hint_text("Reason (optional)")
                        .desired_width(310.0),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        action = Some(DeclineSuggestBoxAction::Cancelled);
                    }
                    if ui
                        .button(egui::RichText::new("Decline").color(colors.attention_fg))
                        .clicked()
                    {
                        let trimmed = self.reason.trim();
                        action = Some(DeclineSuggestBoxAction::ConfirmDecline {
                            msg_id: self.msg_id,
                            comment: (!trimmed.is_empty()).then(|| trimmed.to_owned()),
                        });
                    }
                });
            });
        action
    }
}

// ---------------------------------------------------------------------------
// MonoforumSectionWidget (Channel Direct Messages UI)
// ---------------------------------------------------------------------------

/// Navigation / interaction event emitted by [`MonoforumSectionWidget::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MonoforumSectionAction {
    /// Return to the parent channel history.
    Back,
    /// Show a status toast message.
    Toast(String),
}

/// Stateful Channel Direct Messages (monoforum) section widget: renders both the admin
/// sublist inbox (`messages.getSavedDialogs`) and an individual direct-message / suggested-post
/// thread (`messages.getSavedHistory`, `messages.toggleSuggestedPostApproval`, `messages.sendMessage`).
#[derive(Debug, Default)]
pub struct MonoforumSectionWidget {
    /// Currently selected sublist peer id (`None` = show sublists inbox).
    pub active_sublist: Option<PeerId>,
    /// Search query filtering the monoforum sublists inbox.
    pub search_query: String,
    /// Compose text input inside an open monoforum sublist.
    pub compose_text: String,
    /// Active suggested-post offer attached to the outgoing compose message.
    pub pending_suggest: Option<SuggestOptions>,
    /// Open price/time modal for configuring a suggested post.
    pub suggest_box: Option<SuggestOptionsBoxState>,
    /// Open decline-reason modal for rejecting a suggested post.
    pub decline_box: Option<DeclineSuggestBoxState>,
}

impl MonoforumSectionWidget {
    /// Renders the Channel Direct Messages (monoforum) section inside `rect`.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        monoforum_peer_id: PeerId,
        channel_title: &str,
        is_admin: bool,
        now_unix: i32,
        colors: &HistoryColors,
        backend: &mut dyn Layer233ExtrasBackend,
    ) -> Option<MonoforumSectionAction> {
        let mut action = None;

        // Top bar (54px)
        let top_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 54.0));
        ui.painter()
            .rect_filled(top_rect, CornerRadius::ZERO, colors.bar_bg);
        ui.painter().line_segment(
            [
                Pos2::new(top_rect.left(), top_rect.bottom()),
                Pos2::new(top_rect.right(), top_rect.bottom()),
            ],
            Stroke::new(1.0, colors.divider),
        );

        let sublists = backend.monoforum_sublists(monoforum_peer_id);
        let active_sublist_info = self
            .active_sublist
            .and_then(|sid| sublists.iter().find(|s| s.sublist_peer_id == sid).cloned());

        let header_title = if let Some(ref sub) = active_sublist_info {
            format!("{} — {}", sub.title, channel_title)
        } else {
            format!("Direct Messages • {channel_title}")
        };

        ui.scope_builder(egui::UiBuilder::new().max_rect(top_rect), |ui| {
            ui.horizontal_centered(|ui| {
                ui.add_space(10.0);
                if tg_app_core::ui_icons::back_button(ui, "Back").clicked() {
                    if self.active_sublist.is_some() {
                        self.active_sublist = None;
                    } else {
                        action = Some(MonoforumSectionAction::Back);
                    }
                }
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(header_title)
                        .strong()
                        .size(14.5)
                        .color(colors.bold_fg),
                );
            });
        });

        let body_rect = Rect::from_min_max(Pos2::new(rect.left(), top_rect.bottom()), rect.max);
        ui.painter()
            .rect_filled(body_rect, CornerRadius::ZERO, colors.canvas_bg);

        if let Some(sublist_peer_id) = self.active_sublist {
            // Sublist thread view
            let suggest_bar_h = if self.pending_suggest.is_some() {
                44.0
            } else {
                0.0
            };
            let compose_h = 54.0;
            let list_rect = Rect::from_min_max(
                body_rect.min,
                Pos2::new(
                    body_rect.right(),
                    (body_rect.bottom() - compose_h - suggest_bar_h).max(body_rect.top() + 40.0),
                ),
            );

            let messages: Vec<HistoryMessage> =
                backend.monoforum_sublist_messages(monoforum_peer_id, sublist_peer_id);
            let mut approve_msg: Option<MsgId> = None;
            let mut decline_msg: Option<MsgId> = None;
            let mut change_msg: Option<(MsgId, SuggestedPostData)> = None;

            ui.scope_builder(egui::UiBuilder::new().max_rect(list_rect), |ui| {
                ScrollArea::vertical()
                    .id_salt(("tg_monoforum_msgs", monoforum_peer_id.0, sublist_peer_id.0))
                    .show(ui, |ui| {
                        ui.add_space(8.0);
                        if messages.is_empty() {
                            ui.centered_and_justified(|ui| {
                                ui.label(
                                    egui::RichText::new("No direct messages in this thread yet.")
                                        .color(colors.sub_text_fg),
                                );
                            });
                        }
                        for msg in &messages {
                            ui.horizontal(|ui| {
                                ui.add_space(12.0);
                                ui.vertical(|ui| {
                                    ui.label(
                                        egui::RichText::new(&msg.sender_name)
                                            .strong()
                                            .size(12.5)
                                            .color(colors.active_text_fg),
                                    );
                                    if !msg.text.is_empty() {
                                        ui.label(
                                            egui::RichText::new(&msg.text.text)
                                                .size(13.5)
                                                .color(colors.text_fg),
                                        );
                                    }
                                    if let Some(ref svc) = msg.service {
                                        ui.label(
                                            egui::RichText::new(svc.display_text(&msg.sender_name))
                                                .italics()
                                                .size(12.0)
                                                .color(colors.sub_text_fg),
                                        );
                                    }
                                    if let Some(sp) =
                                        backend.suggested_post_for(monoforum_peer_id, msg.id)
                                    {
                                        let card_w = (list_rect.width() - 40.0).clamp(220.0, 360.0);
                                        let (card_rect, _) = ui.allocate_exact_size(
                                            Vec2::new(card_w, SUGGEST_CARD_HEIGHT),
                                            Sense::hover(),
                                        );
                                        if let Some(ev) = paint_suggested_post_card(
                                            ui, msg.id, &sp, is_admin, card_rect, colors,
                                        ) {
                                            match ev {
                                                SuggestCardAction::Accept(id) => {
                                                    approve_msg = Some(id);
                                                }
                                                SuggestCardAction::Decline(id) => {
                                                    decline_msg = Some(id);
                                                }
                                                SuggestCardAction::SuggestChanges(id) => {
                                                    change_msg = Some((id, sp));
                                                }
                                            }
                                        }
                                    }
                                });
                            });
                            ui.add_space(8.0);
                        }
                    });
            });

            if let Some(mid) = approve_msg {
                match backend.toggle_suggested_post_approval(
                    monoforum_peer_id,
                    mid,
                    SuggestApprovalDecision::Approve {
                        schedule_date: None,
                    },
                ) {
                    Ok(()) => {
                        action = Some(MonoforumSectionAction::Toast(
                            "Suggested post approved.".to_owned(),
                        ));
                    }
                    Err(err) => {
                        action = Some(MonoforumSectionAction::Toast(err));
                    }
                }
            }
            if let Some(mid) = decline_msg {
                self.decline_box = Some(DeclineSuggestBoxState::new(mid));
            }
            if let Some((_mid, sp)) = change_msg {
                let opts = SuggestOptions {
                    price: sp.price,
                    schedule_date: sp.schedule_date,
                };
                self.suggest_box = Some(SuggestOptionsBoxState::new(
                    SuggestMode::Change,
                    is_admin,
                    &opts,
                    now_unix,
                ));
            }

            // Optional SuggestOptionsBar above compose
            if let Some(ref opts) = self.pending_suggest.clone() {
                let bar_rect = Rect::from_min_size(
                    Pos2::new(body_rect.left(), list_rect.bottom()),
                    Vec2::new(body_rect.width(), suggest_bar_h),
                );
                if let Some(bar_ev) =
                    paint_suggest_options_bar(ui, bar_rect, opts, SuggestMode::New, colors)
                {
                    match bar_ev {
                        SuggestOptionsBarAction::Edit => {
                            self.suggest_box = Some(SuggestOptionsBoxState::new(
                                SuggestMode::New,
                                is_admin,
                                opts,
                                now_unix,
                            ));
                        }
                        SuggestOptionsBarAction::Clear => {
                            self.pending_suggest = None;
                        }
                    }
                }
            }

            // Compose bar
            let compose_rect = Rect::from_min_size(
                Pos2::new(body_rect.left(), body_rect.bottom() - compose_h),
                Vec2::new(body_rect.width(), compose_h),
            );
            ui.painter()
                .rect_filled(compose_rect, CornerRadius::ZERO, colors.bar_bg);
            ui.scope_builder(egui::UiBuilder::new().max_rect(compose_rect), |ui| {
                ui.horizontal_centered(|ui| {
                    ui.add_space(10.0);
                    if ui.button("Suggest Post").clicked() {
                        let initial = self.pending_suggest.clone().unwrap_or_default();
                        self.suggest_box = Some(SuggestOptionsBoxState::new(
                            SuggestMode::New,
                            is_admin,
                            &initial,
                            now_unix,
                        ));
                    }
                    ui.add(
                        TextEdit::singleline(&mut self.compose_text)
                            .id(Id::unique("tg_monoforum_compose_input"))
                            .hint_text("Write a direct message or suggested post...")
                            .desired_width((compose_rect.width() - 210.0).max(100.0)),
                    );
                    if ui.button("Send").clicked() {
                        let trimmed = self.compose_text.trim().to_owned();
                        if !trimmed.is_empty() {
                            match backend.send_monoforum_message(
                                monoforum_peer_id,
                                sublist_peer_id,
                                TextWithEntities::simple(trimmed),
                                self.pending_suggest.clone(),
                            ) {
                                Ok(_) => {
                                    self.compose_text.clear();
                                    self.pending_suggest = None;
                                }
                                Err(err) => {
                                    action = Some(MonoforumSectionAction::Toast(err));
                                }
                            }
                        }
                    }
                });
            });
        } else {
            // Sublists inbox view
            ui.scope_builder(egui::UiBuilder::new().max_rect(body_rect), |ui| {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    ui.add(
                        TextEdit::singleline(&mut self.search_query)
                            .id(Id::unique("tg_monoforum_search"))
                            .hint_text("Search direct messages...")
                            .desired_width((body_rect.width() - 24.0).max(120.0)),
                    );
                });
                ui.add_space(6.0);
                let q = self.search_query.trim().to_lowercase();
                ScrollArea::vertical()
                    .id_salt(("tg_monoforum_sublists", monoforum_peer_id.0))
                    .show(ui, |ui| {
                        let filtered: Vec<&MonoforumSublist> = sublists
                            .iter()
                            .filter(|s| {
                                q.is_empty()
                                    || s.title.to_lowercase().contains(&q)
                                    || s.last_preview.to_lowercase().contains(&q)
                            })
                            .collect();
                        if filtered.is_empty() {
                            ui.add_space(24.0);
                            ui.centered_and_justified(|ui| {
                                ui.label(
                                    egui::RichText::new("No channel direct messages yet.")
                                        .color(colors.sub_text_fg),
                                );
                            });
                        }
                        for sub in filtered {
                            let row_w = (body_rect.width() - 16.0).max(160.0);
                            let (row_rect, resp) =
                                ui.allocate_exact_size(Vec2::new(row_w, 56.0), Sense::click());
                            resp.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::Role::Button,
                                    true,
                                    sub.title.clone(),
                                )
                            });
                            if resp.hovered() {
                                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                                ui.painter().rect_filled(
                                    row_rect,
                                    CornerRadius::same(8),
                                    colors.bar_bg_over,
                                );
                            }
                            let avatar_center =
                                Pos2::new(row_rect.left() + 26.0, row_rect.center().y);
                            let rgb = sub.avatar_rgb;
                            ui.painter().circle_filled(
                                avatar_center,
                                18.0,
                                Color32::from_rgb(rgb[0], rgb[1], rgb[2]),
                            );
                            let initial = sub
                                .title
                                .chars()
                                .next()
                                .map(|c| c.to_uppercase().to_string())
                                .unwrap_or_else(|| "?".to_owned());
                            ui.painter().text(
                                avatar_center,
                                Align2::CENTER_CENTER,
                                initial,
                                FontId::proportional(14.0),
                                Color32::WHITE,
                            );

                            let tx = row_rect.left() + 54.0;
                            ui.painter().text(
                                Pos2::new(tx, row_rect.top() + 9.0),
                                Align2::LEFT_TOP,
                                &sub.title,
                                FontId::proportional(13.5),
                                colors.bold_fg,
                            );
                            ui.painter().text(
                                Pos2::new(tx, row_rect.top() + 29.0),
                                Align2::LEFT_TOP,
                                &sub.last_preview,
                                FontId::proportional(12.0),
                                colors.sub_text_fg,
                            );

                            if sub.unread_count > 0 || sub.unread_mark {
                                let badge_text = if sub.unread_count > 0 {
                                    sub.unread_count.to_string()
                                } else {
                                    "•".to_owned()
                                };
                                let badge_rect = Rect::from_min_size(
                                    Pos2::new(row_rect.right() - 34.0, row_rect.center().y - 10.0),
                                    Vec2::new(24.0, 20.0),
                                );
                                ui.painter().rect_filled(
                                    badge_rect,
                                    CornerRadius::same(10),
                                    colors.accent,
                                );
                                ui.painter().text(
                                    badge_rect.center(),
                                    Align2::CENTER_CENTER,
                                    badge_text,
                                    FontId::proportional(11.0),
                                    Color32::WHITE,
                                );
                            }

                            if resp.clicked() {
                                self.active_sublist = Some(sub.sublist_peer_id);
                                if sub.top_message_id.0 > 0 {
                                    let _ = backend.mark_monoforum_sublist_read(
                                        monoforum_peer_id,
                                        sub.sublist_peer_id,
                                        sub.top_message_id,
                                    );
                                }
                            }
                        }
                    });
            });
        }

        // Modals
        if let Some(ref mut box_state) = self.suggest_box
            && let Some(ev) = box_state.show(ui, colors)
        {
            match ev {
                SuggestOptionsBoxAction::Confirmed(opts) => {
                    self.pending_suggest = Some(opts);
                    self.suggest_box = None;
                }
                SuggestOptionsBoxAction::Cancelled => {
                    self.suggest_box = None;
                }
            }
        }
        if let Some(ref mut decline_state) = self.decline_box
            && let Some(ev) = decline_state.show(ui, colors)
        {
            match ev {
                DeclineSuggestBoxAction::ConfirmDecline { msg_id, comment } => {
                    match backend.toggle_suggested_post_approval(
                        monoforum_peer_id,
                        msg_id,
                        SuggestApprovalDecision::Reject { comment },
                    ) {
                        Ok(()) => {
                            action = Some(MonoforumSectionAction::Toast(
                                "Suggested post declined.".to_owned(),
                            ));
                        }
                        Err(err) => {
                            action = Some(MonoforumSectionAction::Toast(err));
                        }
                    }
                    self.decline_box = None;
                }
                DeclineSuggestBoxAction::Cancelled => {
                    self.decline_box = None;
                }
            }
        }

        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commission_and_ton_parsing_match_tdesktop() {
        let stars = SuggestedPrice::stars(1000);
        assert_eq!(
            stars.after_commission(SUGGESTED_POST_COMMISSION_STARS),
            SuggestedPrice::stars(850)
        );
        assert_eq!(SuggestedPrice::format_commission_percent(850), "85%");
        assert_eq!(SuggestedPrice::format_commission_percent(855), "85.5%");

        let ton_nanos = SuggestedPrice::parse_ton_nanos("1.5").expect("parse 1.5 TON");
        assert_eq!(ton_nanos, 1_500_000_000);
        let ton = SuggestedPrice::ton_nanos(ton_nanos);
        assert_eq!(ton.format_label(), "💎 1.5 TON");
        let after = ton.after_commission(SUGGESTED_POST_COMMISSION_TON);
        assert_eq!(after.total_nanos(), 1_275_000_000);
    }

    #[test]
    fn monoforum_reply_required_matches_chat_section_monoforum() {
        assert!(monoforum_reply_required(true, false, false, false));
        assert!(!monoforum_reply_required(true, false, false, true));
        assert!(!monoforum_reply_required(true, false, true, false));
        assert!(!monoforum_reply_required(true, true, false, false));
        assert!(!monoforum_reply_required(false, false, false, false));
    }

    #[test]
    fn sublist_read_till_clears_unread_count() {
        let mut sub = MonoforumSublist {
            parent_peer_id: PeerId::from_channel(3099),
            sublist_peer_id: PeerId::from_user(1001),
            title: "Mira Vance".to_owned(),
            avatar_rgb: [90, 140, 210],
            top_message_id: MsgId(20),
            last_preview: "Suggested post".to_owned(),
            last_date_unix: 1_795_341_600,
            read_inbox_max_id: MsgId(10),
            read_outbox_max_id: MsgId(10),
            unread_count: 3,
            unread_reactions_count: 0,
            unread_mark: true,
            nopaid_messages_exception: false,
            draft_text: None,
        };
        sub.apply_read_till(MsgId(20), false);
        assert_eq!(sub.read_inbox_max_id, MsgId(20));
        assert_eq!(sub.unread_count, 0);
        assert!(!sub.unread_mark);
    }
}
