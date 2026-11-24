//! Join Requests approval box (`JoinRequestsBox`).
//!
//! Port origin:
//! - `Telegram/SourceFiles/boxes/peers/edit_peer_requests_box.cpp`
//! - `Telegram/SourceFiles/info/requests_list/info_requests_list_widget.cpp`

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Role, Sense, Stroke, Ui, Vec2,
    WidgetInfo,
};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold, font_id_from_style};
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{Colors, InputFieldStyle};
use tg_ui::tg_style::st;
use tg_ui::userpic::EmptyUserpic;
use tg_ui::widgets::input_field::InputField;

use crate::info::backend::{InfoBackend, InviteImporterInfo};
use tg_app_core::history::backend::HistoryBackend;
use tg_app_core::stand_in::types::PeerId;

/// Events emitted by [`JoinRequestsBoxState`] body actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinRequestsAction {
    /// A single user request was approved or dismissed.
    SingleResolved {
        /// Requester user peer id.
        user_id: PeerId,
        /// `true` if approved, `false` if dismissed.
        approved: bool,
    },
    /// All pending requests were approved or dismissed in bulk.
    AllResolved {
        /// Number of requests resolved.
        count: usize,
        /// `true` if approved, `false` if dismissed.
        approved: bool,
    },
}

/// Modal state for reviewing, searching, approving, and dismissing chat join requests.
#[derive(Debug, Clone)]
pub struct JoinRequestsBoxState {
    /// Target group or channel peer identifier.
    pub peer_id: PeerId,
    /// `true` for a group/supergroup, `false` for a broadcast channel.
    pub is_group: bool,
    /// Search query passed to `messages.getChatInviteImporters(requested: true, q)`.
    pub search_query: String,
    /// Last searched query to detect changes.
    last_query: String,
    /// Loaded pending join requests.
    pub requests: Vec<InviteImporterInfo>,
    /// Whether initial data has been loaded.
    pub loaded: bool,
    /// Last inline error message.
    pub error: Option<String>,
}

impl JoinRequestsBoxState {
    /// Creates a new join-requests modal for `peer_id`.
    #[must_use]
    pub fn new(peer_id: PeerId, is_group: bool) -> Self {
        Self {
            peer_id,
            is_group,
            search_query: String::new(),
            last_query: String::new(),
            requests: Vec::new(),
            loaded: false,
            error: None,
        }
    }

    /// Loads pending join requests from an [`InfoBackend`].
    pub fn reload(&mut self, backend: &dyn InfoBackend) {
        match backend.chat_invite_importers(self.peer_id, None, true, &self.search_query) {
            Ok(res) => {
                self.requests = res.importers;
                self.error = None;
            }
            Err(err) => {
                self.error = Some(err);
            }
        }
        self.last_query = self.search_query.clone();
        self.loaded = true;
    }

    /// Loads pending join requests from a [`HistoryBackend`] (used by the in-chat bar overlay).
    pub fn reload_from_history(&mut self, backend: &dyn HistoryBackend) {
        self.requests = backend.join_requests_entries(self.peer_id, &self.search_query);
        self.last_query = self.search_query.clone();
        self.loaded = true;
    }

    /// Approves or dismisses one requester through [`InfoBackend`].
    pub fn decide_one(
        &mut self,
        user_id: PeerId,
        approved: bool,
        backend: &mut dyn InfoBackend,
    ) -> Result<(), String> {
        match backend.hide_chat_join_request(self.peer_id, user_id, approved) {
            Ok(()) => {
                self.requests.retain(|r| r.user_id != user_id);
                self.error = None;
                Ok(())
            }
            Err(err) => {
                self.error = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Approves or dismisses all pending requests through [`InfoBackend`].
    pub fn decide_all(
        &mut self,
        approved: bool,
        backend: &mut dyn InfoBackend,
    ) -> Result<usize, String> {
        match backend.hide_all_chat_join_requests(self.peer_id, None, approved) {
            Ok(n) => {
                self.requests.clear();
                self.error = None;
                Ok(n)
            }
            Err(err) => {
                self.error = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        let mut modal = BoxModal::new(id, tr::lng_manage_peer_requests())
            .width(st::box_wide_width.0 as f32 + 24.0)
            .add_button(BoxButton::default_btn("close", tr::lng_close()));
        if !self.requests.is_empty() {
            modal = modal
                .with_left_button(BoxButton::attention("dismiss_all", "Dismiss All"))
                .add_button(BoxButton::default_btn("approve_all", "Approve All"));
        }
        modal
    }

    /// Renders the join-requests modal body wired to [`InfoBackend`].
    pub fn show_body(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn InfoBackend,
        colors: &dyn Colors,
    ) -> Option<JoinRequestsAction> {
        if !self.loaded || self.search_query != self.last_query {
            self.reload(backend);
        }

        let mut action = None;
        let accept_label = if self.is_group {
            "Add to Group"
        } else {
            "Add to Channel"
        };

        let pending_decision = self.render_common_list(ui, accept_label, false, colors);
        if let Some(dec) = pending_decision {
            match dec {
                ListDecision::One(uid, approved) => {
                    if self.decide_one(uid, approved, backend).is_ok() {
                        action = Some(JoinRequestsAction::SingleResolved {
                            user_id: uid,
                            approved,
                        });
                    }
                }
                ListDecision::All(approved) => {
                    if let Ok(count) = self.decide_all(approved, backend) {
                        action = Some(JoinRequestsAction::AllResolved { count, approved });
                    }
                }
            }
        }
        action
    }

    /// Renders the join-requests body wired to [`HistoryBackend`] (for the in-chat overlay).
    pub fn show_body_history(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn HistoryBackend,
        colors: &dyn Colors,
    ) -> Option<JoinRequestsAction> {
        if !self.loaded || self.search_query != self.last_query {
            self.reload_from_history(backend);
        }

        let mut action = None;
        let accept_label = if self.is_group {
            "Add to Group"
        } else {
            "Add to Channel"
        };

        let pending_decision = self.render_common_list(ui, accept_label, true, colors);
        if let Some(dec) = pending_decision {
            match dec {
                ListDecision::One(uid, approved) => {
                    match backend.decide_join_request(self.peer_id, uid, approved) {
                        Ok(()) => {
                            self.requests.retain(|r| r.user_id != uid);
                            self.error = None;
                            action = Some(JoinRequestsAction::SingleResolved {
                                user_id: uid,
                                approved,
                            });
                        }
                        Err(err) => self.error = Some(err),
                    }
                }
                ListDecision::All(approved) => {
                    match backend.decide_all_join_requests(self.peer_id, approved) {
                        Ok(count) => {
                            self.requests.clear();
                            self.error = None;
                            action = Some(JoinRequestsAction::AllResolved { count, approved });
                        }
                        Err(err) => self.error = Some(err),
                    }
                }
            }
        }
        action
    }

    fn render_common_list(
        &mut self,
        ui: &mut Ui,
        accept_label: &str,
        show_inline_bulk: bool,
        colors: &dyn Colors,
    ) -> Option<ListDecision> {
        let mut decision = None;

        InputField::single_line(
            "join_requests_search",
            &mut self.search_query,
            "Search requests",
            InputFieldStyle::default_input(colors),
        )
        .show(ui);
        ui.add_space(6.0);

        if !self.requests.is_empty() {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("{} pending request(s)", self.requests.len()))
                        .font(FontId::new(12.0, family_regular()))
                        .color(colors.color("windowSubTextFg")),
                );
                if show_inline_bulk {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if bulk_pill(
                            ui,
                            "jr_inline_dismiss_all",
                            "Dismiss All",
                            colors.color("windowBgOver"),
                            colors.color("attentionButtonFg"),
                        ) {
                            decision = Some(ListDecision::All(false));
                        }
                        ui.add_space(6.0);
                        if bulk_pill(
                            ui,
                            "jr_inline_approve_all",
                            "Approve All",
                            colors.color("activeButtonBg"),
                            Color32::WHITE,
                        ) {
                            decision = Some(ListDecision::All(true));
                        }
                    });
                }
            });
            ui.add_space(6.0);
        }

        if self.requests.is_empty() {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new("No pending join requests.")
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("windowSubTextFg")),
                );
            });
            ui.add_space(20.0);
        } else {
            let snapshot = self.requests.clone();
            for req in &snapshot {
                if let Some(approved) = render_request_card(ui, req, accept_label, colors) {
                    decision = Some(ListDecision::One(req.user_id, approved));
                }
            }
        }

        if let Some(ref err) = self.error {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(err)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("boxTextFgError")),
            );
        }

        decision
    }
}

enum ListDecision {
    One(PeerId, bool),
    All(bool),
}

fn render_request_card(
    ui: &mut Ui,
    req: &InviteImporterInfo,
    accept_label: &str,
    colors: &dyn Colors,
) -> Option<bool> {
    let mut result = None;
    let width = ui.available_width().max(220.0);
    let has_bio = req.about.as_deref().is_some_and(|b| !b.is_empty());
    let row_h = if has_bio { 94.0 } else { 76.0 };
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, row_h), Sense::hover());

    let av_rect = Rect::from_center_size(
        Pos2::new(rect.min.x + 22.0, rect.min.y + 26.0),
        Vec2::splat(40.0),
    );
    EmptyUserpic::for_color_index(req.color_index, &req.name, colors)
        .paint_circle(ui.painter(), av_rect);

    let text_x = av_rect.max.x + 12.0;
    let name_line = if let Some(ref uname) = req.username {
        format!("{} (@{uname})", req.name)
    } else {
        req.name.clone()
    };
    ui.painter().text(
        Pos2::new(text_x, rect.min.y + 8.0),
        Align2::LEFT_TOP,
        name_line,
        font_id_from_style(st::semibold_font),
        colors.color("windowBoldFg"),
    );

    if let Some(ref about) = req.about
        && !about.is_empty()
    {
        ui.painter().text(
            Pos2::new(text_x, rect.min.y + 28.0),
            Align2::LEFT_TOP,
            about,
            FontId::new(12.0, family_regular()),
            colors.color("windowSubTextFg"),
        );
    }

    let btn_y = rect.max.y - 34.0;
    let acc_rect = Rect::from_min_size(Pos2::new(text_x, btn_y), Vec2::new(112.0, 26.0));
    let rej_rect = Rect::from_min_size(
        Pos2::new(acc_rect.max.x + 8.0, btn_y),
        Vec2::new(76.0, 26.0),
    );

    let acc_resp = ui.interact(
        acc_rect,
        ui.make_persistent_id(("jr_accept", req.user_id.0)),
        Sense::click(),
    );
    acc_resp.widget_info(|| {
        WidgetInfo::labeled(Role::Button, true, format!("{accept_label} {}", req.name))
    });
    if acc_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    ui.painter().rect_filled(
        acc_rect,
        CornerRadius::same(13),
        if acc_resp.hovered() {
            colors.color("activeButtonBgOver")
        } else {
            colors.color("activeButtonBg")
        },
    );
    ui.painter().text(
        acc_rect.center(),
        Align2::CENTER_CENTER,
        accept_label,
        FontId::new(11.5, family_semibold()),
        Color32::WHITE,
    );
    if acc_resp.clicked() {
        result = Some(true);
    }

    let rej_resp = ui.interact(
        rej_rect,
        ui.make_persistent_id(("jr_reject", req.user_id.0)),
        Sense::click(),
    );
    rej_resp
        .widget_info(|| WidgetInfo::labeled(Role::Button, true, format!("Dismiss {}", req.name)));
    if rej_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    ui.painter().rect_filled(
        rej_rect,
        CornerRadius::same(13),
        colors.color("windowBgOver"),
    );
    ui.painter().text(
        rej_rect.center(),
        Align2::CENTER_CENTER,
        "Dismiss",
        FontId::new(11.5, family_semibold()),
        colors.color("attentionButtonFg"),
    );
    if rej_resp.clicked() {
        result = Some(false);
    }

    ui.painter().line_segment(
        [
            Pos2::new(text_x, rect.max.y),
            Pos2::new(rect.max.x, rect.max.y),
        ],
        Stroke::new(0.5, colors.color("shadowFg")),
    );

    result
}

fn bulk_pill(ui: &mut Ui, _id_salt: &str, label: &str, bg: Color32, fg: Color32) -> bool {
    let font = FontId::new(11.5, family_semibold());
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, fg);
    let size = Vec2::new(galley.size().x + 16.0, 24.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    ui.painter().rect_filled(rect, CornerRadius::same(6), bg);
    ui.painter().galley(
        Pos2::new(
            rect.center().x - galley.size().x * 0.5,
            rect.center().y - galley.size().y * 0.5,
        ),
        galley,
        fg,
    );
    resp.clicked()
}
