//! "Join Voice Chat as..." / "Display Me As..." peer selection dialog.
//!
//! Port of `Telegram/SourceFiles/calls/group/calls_choose_join_as.cpp`.

use egui::{
    Align2, Color32, CornerRadius, FontId, Pos2, RichText, ScrollArea, Sense, Stroke, Ui, Vec2,
};
use tg_lang::tr;

use crate::calls::backend::JoinAsCandidate;
use crate::calls::theme::{CallPalette, paint_avatar, paint_check_icon};
use tg_app_core::stand_in::types::PeerId;

/// Context in which the Join-As selector is opened (`ChooseJoinAsProcess::Context`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChooseJoinAsContext {
    /// Starting a new voice chat or live stream.
    Create,
    /// Joining an existing voice chat or live stream.
    Join,
    /// Switching identity inside an already-joined group call.
    Switch,
}

/// Modal dialog for selecting which personal account or managed channel to appear as in a group call.
#[derive(Debug)]
pub struct ChooseJoinAsBox {
    /// Whether the dialog is open.
    pub open: bool,
    /// Target group/channel peer ID.
    pub group_peer_id: PeerId,
    /// Whether the target peer is a broadcast channel (live stream).
    pub is_broadcast: bool,
    /// Creation / Join / Switch context.
    pub context: ChooseJoinAsContext,
    /// Candidate peers offered by the backend.
    pub candidates: Vec<JoinAsCandidate>,
    /// Currently selected `join_as` peer ID.
    pub selected: PeerId,
}

impl Default for ChooseJoinAsBox {
    fn default() -> Self {
        Self {
            open: false,
            group_peer_id: PeerId::ZERO,
            is_broadcast: false,
            context: ChooseJoinAsContext::Join,
            candidates: Vec::new(),
            selected: PeerId::ZERO,
        }
    }
}

/// Result emitted when the user confirms [`ChooseJoinAsBox`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChooseJoinAsConfirmed {
    /// Target group/channel peer ID.
    pub group_peer_id: PeerId,
    /// Selected `join_as` identity peer ID.
    pub join_as: PeerId,
    /// Context under which the selection was made.
    pub context: ChooseJoinAsContext,
}

impl ChooseJoinAsBox {
    /// Opens the dialog populated with `candidates` and `initial_selected`.
    pub fn open_for(
        &mut self,
        group_peer_id: PeerId,
        is_broadcast: bool,
        context: ChooseJoinAsContext,
        candidates: Vec<JoinAsCandidate>,
        initial_selected: PeerId,
    ) {
        self.group_peer_id = group_peer_id;
        self.is_broadcast = is_broadcast;
        self.context = context;
        self.selected = candidates
            .iter()
            .find(|c| c.peer.id == initial_selected)
            .or_else(|| candidates.first())
            .map_or(PeerId::ZERO, |c| c.peer.id);
        self.candidates = candidates;
        self.open = true;
    }

    /// Returns the localized dialog title (`ChooseJoinAsBox`, `calls_choose_join_as.cpp:193-205`).
    #[must_use]
    pub fn title(&self) -> String {
        match self.context {
            ChooseJoinAsContext::Create => {
                if self.is_broadcast {
                    tr::lng_group_call_start_as_header_channel()
                } else {
                    tr::lng_group_call_start_as_header()
                }
            }
            ChooseJoinAsContext::Join => {
                if self.is_broadcast {
                    tr::lng_group_call_join_as_header_channel()
                } else {
                    tr::lng_group_call_join_as_header()
                }
            }
            ChooseJoinAsContext::Switch => tr::lng_group_call_display_as_header(),
        }
    }

    /// Renders the modal window when open, returning `Some(ChooseJoinAsConfirmed)` on confirmation.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        palette: &CallPalette,
    ) -> Option<ChooseJoinAsConfirmed> {
        if !self.open {
            return None;
        }
        let mut confirmed = None;
        let mut keep_open = self.open;
        let mut close_requested = false;
        let dark = self.context == ChooseJoinAsContext::Switch;
        let title = self.title();

        let mut win = egui::Window::new(title)
            .id(egui::Id::unique("calls_choose_join_as_box"))
            .collapsible(false)
            .resizable(false)
            .fixed_size(Vec2::new(330.0, 340.0))
            .open(&mut keep_open);

        if dark {
            win = win.frame(
                egui::Frame::new()
                    .fill(palette.group_members_bg)
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(egui::Margin::same(16)),
            );
        }

        win.show(ctx, |ui| {
            let text_fg = if dark {
                palette.group_members_fg
            } else {
                ui.visuals().text_color()
            };
            let sub_fg = if dark {
                palette.group_member_sub_fg
            } else {
                ui.visuals().weak_text_color()
            };

            ui.label(
                RichText::new(tr::lng_group_call_join_as_about())
                    .size(12.5)
                    .color(sub_fg),
            );
            ui.add_space(10.0);

            ScrollArea::vertical().max_height(210.0).show(ui, |ui| {
                for cand in &self.candidates {
                    let is_selected = cand.peer.id == self.selected;
                    if show_candidate_row(ui, cand, is_selected, dark, text_fg, sub_fg, palette) {
                        self.selected = cand.peer.id;
                    }
                }
            });

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let next_label = if self.context == ChooseJoinAsContext::Switch {
                        tr::lng_settings_save()
                    } else {
                        tr::lng_continue()
                    };
                    if ui.button(next_label).clicked() {
                        confirmed = Some(ChooseJoinAsConfirmed {
                            group_peer_id: self.group_peer_id,
                            join_as: self.selected,
                            context: self.context,
                        });
                        close_requested = true;
                    }
                    if ui.button(tr::lng_cancel()).clicked() {
                        close_requested = true;
                    }
                });
            });
        });

        self.open = keep_open && !close_requested;
        confirmed
    }
}

fn show_candidate_row(
    ui: &mut Ui,
    cand: &JoinAsCandidate,
    selected: bool,
    dark: bool,
    text_fg: Color32,
    sub_fg: Color32,
    palette: &CallPalette,
) -> bool {
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 52.0), Sense::click());
    let painter = ui.painter();
    if resp.hovered() {
        let hover_bg = if dark {
            palette.group_members_bg_over
        } else {
            ui.visuals().faint_bg_color
        };
        painter.rect_filled(rect, CornerRadius::same(8), hover_bg);
    }

    let avatar_center = Pos2::new(rect.left() + 26.0, rect.center().y);
    paint_avatar(
        painter,
        avatar_center,
        19.0,
        cand.peer.avatar_color,
        &cand.peer.initials,
        13.0,
    );

    // Selected radio checkmark badge (`peerListSetRowChecked`).
    if selected {
        let badge = Pos2::new(avatar_center.x + 14.0, avatar_center.y + 13.0);
        painter.circle_filled(badge, 8.5, palette.group_active_fg);
        painter.circle_stroke(
            badge,
            8.5,
            Stroke::new(
                1.5,
                if dark {
                    palette.group_members_bg
                } else {
                    ui.visuals().window_fill
                },
            ),
        );
        paint_check_icon(painter, badge, 11.0, Color32::WHITE);
    }

    painter.text(
        Pos2::new(rect.left() + 56.0, rect.top() + 17.0),
        Align2::LEFT_CENTER,
        &cand.peer.name,
        FontId::proportional(13.5),
        text_fg,
    );
    painter.text(
        Pos2::new(rect.left() + 56.0, rect.top() + 35.0),
        Align2::LEFT_CENTER,
        &cand.subtitle,
        FontId::proportional(11.5),
        sub_fg,
    );

    resp.clicked()
}
