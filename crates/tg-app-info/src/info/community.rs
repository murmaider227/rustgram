//! Community Info panel sections (`info/community/*` & `info/community_requests/*`).
//!
//! Port origin:
//! - `Telegram/SourceFiles/info/community/info_community_widget.h` / `.cpp`
//! - `Telegram/SourceFiles/info/community_requests/info_community_requests_widget.h` / `.cpp`

use egui::{CornerRadius, FontId, Pos2, Rect, Sense, Ui, Vec2};
use tg_ui::fonts::{family_regular, family_semibold};
use tg_ui::stand_in::{Colors, FlatLabelStyle, ToggleStyle};
use tg_ui::userpic::EmptyUserpic;
use tg_ui::widgets::checkbox::Toggle;
use tg_ui::widgets::labels::{FlatLabel, divider_bar};

use crate::boxes::community::{
    CommunityDetails, CommunityPeerLinkAction, CommunityPeerRequestsSlice,
};
use crate::dialogs::community::{
    CommunityChatsKind, CommunityChatsListAction, CommunityChatsListWidget,
};
use crate::info::backend::InfoBackend;
use tg_app_core::stand_in::types::PeerId;

/// Events emitted by [`CommunityInfoSection`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommunityInfoAction {
    /// Open a joined chat or bot history.
    OpenPeer(PeerId),
    /// Open Pending Link Requests subsection.
    OpenRequests(PeerId),
    /// Open Manage Community modal.
    OpenManage(PeerId),
    /// Open Add Chat to Community modal.
    OpenAddChat(PeerId),
    /// Toggled "Show as one chat" collapsed grouping.
    CollapsedToggled(bool),
    /// Removed a linked peer from the community.
    PeerRemoved(PeerId),
    /// Display a toast notification.
    Toast(String),
}

/// Community overview section rendered inside [`crate::info::wrap_widget::InfoWrapWidget`]
/// (`Info::Community::Widget`).
pub struct CommunityInfoSection;

impl CommunityInfoSection {
    /// Renders the community header, collapse toggle, pending link requests row,
    /// joined/discoverable chat lists, bots list, and bottom "Add Chat" button.
    pub fn show(
        ui: &mut Ui,
        details: &CommunityDetails,
        backend: &mut dyn InfoBackend,
        colors: &dyn Colors,
    ) -> Option<CommunityInfoAction> {
        let mut action = None;

        // Top flexible header summary
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            let (av_rect, _) = ui.allocate_exact_size(Vec2::splat(56.0), Sense::hover());
            EmptyUserpic::for_peer(details.id.bare(), &details.title, colors)
                .paint_rounded_or_photo(ui.painter(), av_rect, 14.0, details.id.0);
            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(&details.title)
                        .font(FontId::new(16.0, family_semibold()))
                        .color(colors.color("windowBoldFg")),
                );
                let count = details.linked_peers.len();
                let chats_status = if count == 1 {
                    "1 chat".to_owned()
                } else {
                    format!("{count} chats")
                };
                ui.label(
                    egui::RichText::new(chats_status)
                        .font(FontId::new(13.0, family_regular()))
                        .color(colors.color("windowSubTextFg")),
                );
                if !details.about.is_empty() {
                    ui.label(
                        egui::RichText::new(&details.about)
                            .font(FontId::new(12.5, family_regular()))
                            .color(colors.color("windowFg")),
                    );
                }
            });
        });

        ui.add_space(10.0);
        divider_bar(ui, 8.0, colors);
        ui.add_space(8.0);

        // "Show as one chat" collapsed-in-dialogs toggle
        let mut collapsed = details.collapsed_in_dialogs;
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            ui.vertical(|ui| {
                if Toggle::new(&mut collapsed, ToggleStyle::default_toggle(colors))
                    .with_label("Show as one chat", colors.color("windowFg"))
                    .show(ui)
                    .changed()
                {
                    match backend.toggle_community_collapsed_in_dialogs(details.id, collapsed) {
                        Ok(()) => action = Some(CommunityInfoAction::CollapsedToggled(collapsed)),
                        Err(err) => action = Some(CommunityInfoAction::Toast(err)),
                    }
                }
                ui.label(
                    egui::RichText::new(
                        "Combine joined chats from this community into one grouped row.",
                    )
                    .font(FontId::new(12.0, family_regular()))
                    .color(colors.color("windowSubTextFg")),
                );
            });
        });

        // Pending link requests row (visible to admins with manage_linked_peers)
        if details.can_manage_linked_peers && details.pending_requests_count > 0 {
            ui.add_space(8.0);
            divider_bar(ui, 8.0, colors);
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                if ui
                    .button(format!(
                        "Pending Link Requests ({})",
                        details.pending_requests_count
                    ))
                    .clicked()
                {
                    action = Some(CommunityInfoAction::OpenRequests(details.id));
                }
            });
        }

        // Joined chats list (`Dialogs::CommunityChatsList` — `Joined`)
        let joined = details.joined_chats();
        if !joined.is_empty() {
            ui.add_space(10.0);
            divider_bar(ui, 8.0, colors);
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                FlatLabel::plain("Your Chats", FlatLabelStyle::subsection_title(colors))
                    .id_salt("info_comm_joined")
                    .show(ui);
            });
            if let Some(ev) = CommunityChatsListWidget::show(
                ui,
                "info_comm_joined_list",
                CommunityChatsKind::Joined,
                &joined,
                details.can_manage_linked_peers,
                colors,
            ) {
                action = handle_chats_list_action(details.id, ev, backend);
            }
        }

        // Discoverable chats list (`Dialogs::CommunityChatsList` — `Discoverable`)
        let discoverable = details.discoverable_chats();
        if !discoverable.is_empty() {
            ui.add_space(10.0);
            divider_bar(ui, 8.0, colors);
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                FlatLabel::plain("More Chats", FlatLabelStyle::subsection_title(colors))
                    .id_salt("info_comm_more")
                    .show(ui);
            });
            if let Some(ev) = CommunityChatsListWidget::show(
                ui,
                "info_comm_more_list",
                CommunityChatsKind::Discoverable,
                &discoverable,
                details.can_manage_linked_peers,
                colors,
            ) {
                action = handle_chats_list_action(details.id, ev, backend);
            }
        }

        // Bots list
        let bots = details.linked_bots();
        if !bots.is_empty() {
            ui.add_space(10.0);
            divider_bar(ui, 8.0, colors);
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                FlatLabel::plain("Bots", FlatLabelStyle::subsection_title(colors))
                    .id_salt("info_comm_bots")
                    .show(ui);
            });
            if let Some(ev) = CommunityChatsListWidget::show(
                ui,
                "info_comm_bots_list",
                CommunityChatsKind::Bots,
                &bots,
                details.can_manage_linked_peers,
                colors,
            ) {
                action = handle_chats_list_action(details.id, ev, backend);
            }
        }

        // Bottom Add Chat & Manage Community actions (`info_community_widget.cpp:setupAddChatButton`)
        if details.can_manage_linked_peers || details.can_edit {
            ui.add_space(12.0);
            divider_bar(ui, 8.0, colors);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                if details.can_manage_linked_peers && ui.button("+ Add Chat").clicked() {
                    action = Some(CommunityInfoAction::OpenAddChat(details.id));
                }
                if details.can_edit && ui.button("Manage Community").clicked() {
                    action = Some(CommunityInfoAction::OpenManage(details.id));
                }
            });
        }

        ui.add_space(16.0);
        action
    }
}

fn handle_chats_list_action(
    community_id: PeerId,
    ev: CommunityChatsListAction,
    backend: &mut dyn InfoBackend,
) -> Option<CommunityInfoAction> {
    match ev {
        CommunityChatsListAction::OpenPeer(pid) => Some(CommunityInfoAction::OpenPeer(pid)),
        CommunityChatsListAction::HiddenInaccessible => Some(CommunityInfoAction::Toast(
            "This chat is hidden and not accessible.".to_owned(),
        )),
        CommunityChatsListAction::RemovePeer(pid) => {
            match backend.toggle_community_peer_link(
                community_id,
                pid,
                CommunityPeerLinkAction::Deleted,
            ) {
                Ok(()) => Some(CommunityInfoAction::PeerRemoved(pid)),
                Err(err) => Some(CommunityInfoAction::Toast(err)),
            }
        }
    }
}

/// Actions emitted by [`CommunityRequestsSection`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommunityRequestsSectionAction {
    /// Approve a single pending link request.
    Approve(PeerId),
    /// Reject a single pending link request.
    Reject(PeerId),
    /// Approve all pending link requests.
    ApproveAll,
    /// Reject all pending link requests.
    RejectAll,
}

/// Community pending peer link requests section
/// (`info/community_requests/info_community_requests_widget.cpp`).
pub struct CommunityRequestsSection;

impl CommunityRequestsSection {
    /// Renders all pending peer link requests inside `InfoWrapWidget`.
    pub fn show(
        ui: &mut Ui,
        slice: &CommunityPeerRequestsSlice,
        colors: &dyn Colors,
    ) -> Option<CommunityRequestsSectionAction> {
        let mut action = None;
        ui.add_space(10.0);

        if slice.list.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                FlatLabel::plain(
                    "No pending community link requests.",
                    FlatLabelStyle::box_label(colors),
                )
                .id_salt("info_comm_reqs_empty")
                .show(ui);
            });
            return None;
        }

        ui.horizontal(|ui| {
            ui.add_space(16.0);
            if ui.button("Approve All").clicked() {
                action = Some(CommunityRequestsSectionAction::ApproveAll);
            }
            if ui.button("Reject All").clicked() {
                action = Some(CommunityRequestsSectionAction::RejectAll);
            }
        });
        ui.add_space(8.0);
        divider_bar(ui, 6.0, colors);
        ui.add_space(6.0);

        for req in &slice.list {
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                let (av_rect, _) = ui.allocate_exact_size(Vec2::splat(38.0), Sense::hover());
                EmptyUserpic::for_peer(req.peer_id.bare(), &req.peer_title, colors)
                    .paint_circle(ui.painter(), av_rect);
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(&req.peer_title)
                            .font(FontId::new(13.5, family_semibold()))
                            .color(colors.color("windowBoldFg")),
                    );
                    let vis = if req.visible { "Visible" } else { "Hidden" };
                    let by = if req.requested_by_name.is_empty() {
                        vis.to_owned()
                    } else {
                        format!("By {} · {vis}", req.requested_by_name)
                    };
                    ui.label(
                        egui::RichText::new(by)
                            .font(FontId::new(12.0, family_regular()))
                            .color(colors.color("windowSubTextFg")),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(12.0);
                    if ui.small_button("Reject").clicked() {
                        action = Some(CommunityRequestsSectionAction::Reject(req.peer_id));
                    }
                    if ui.small_button("Approve").clicked() {
                        action = Some(CommunityRequestsSectionAction::Approve(req.peer_id));
                    }
                });
            });
            let sep_rect = Rect::from_min_size(
                Pos2::new(ui.min_rect().left() + 16.0, ui.cursor().top() + 4.0),
                Vec2::new((ui.available_width() - 32.0).max(40.0), 1.0),
            );
            ui.painter()
                .rect_filled(sep_rect, CornerRadius::ZERO, colors.color("shadowFg"));
            ui.add_space(8.0);
        }

        action
    }
}
