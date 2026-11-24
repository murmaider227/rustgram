//! Communities (Subgroups) modal dialogs: Community overview, Create Community,
//! Manage Community, Pending Peer Link Requests, and Add Chat to Community.
//!
//! Port origin:
//! - `Telegram/SourceFiles/api/api_communities.h` / `.cpp`
//! - `Telegram/SourceFiles/data/data_community.h` / `.cpp`
//! - `Telegram/SourceFiles/boxes/peers/community_box.cpp`
//! - `Telegram/SourceFiles/boxes/peers/manage_community_box.cpp`
//! - `Telegram/SourceFiles/boxes/peers/community_pending_requests_box.cpp`
//! - `Telegram/SourceFiles/boxes/peers/add_to_community_box.cpp`

use egui::{Align2, CornerRadius, CursorIcon, FontId, Pos2, Rect, Sense, Ui, Vec2};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold, font_id_from_style};
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{
    CheckboxStyle, Colors, FlatLabelStyle, InputFieldStyle, RadioStyle, ToggleStyle,
};
use tg_ui::tg_style::st;
use tg_ui::userpic::EmptyUserpic;
use tg_ui::widgets::checkbox::{Radio, Toggle};
use tg_ui::widgets::input_field::InputField;
use tg_ui::widgets::labels::{FlatLabel, divider_bar};

use crate::info::backend::InfoBackend;
use crate::info::values::{MAX_CHANNEL_DESCRIPTION, MAX_GROUP_CHANNEL_TITLE};
use tg_app_core::stand_in::types::PeerId;

pub use tg_app_core::core_types::dto::community::*;

/// Community overview modal (`boxes/peers/community_box.cpp`).
#[derive(Debug, Clone)]
pub struct CommunityBoxState {
    /// Community peer ID.
    pub community_id: PeerId,
    /// Cached community title for modal heading.
    pub title: String,
    /// Optional error banner.
    pub error: Option<String>,
    /// Peer awaiting join confirmation inside the box (if user clicked a discoverable chat).
    pub pending_join_peer: Option<(PeerId, String)>,
}

impl CommunityBoxState {
    /// Creates a new [`CommunityBoxState`] for `community_id`.
    #[must_use]
    pub fn new(community_id: PeerId, title: impl Into<String>) -> Self {
        Self {
            community_id,
            title: title.into(),
            error: None,
            pending_join_peer: None,
        }
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        let heading = if self.title.is_empty() {
            "Community".to_owned()
        } else {
            self.title.clone()
        };
        BoxModal::new(id, heading)
            .width(st::box_wide_width.0 as f32 + 24.0)
            .add_button(BoxButton::default_btn("close", tr::lng_close()))
    }

    /// Renders the community overview body (collapse toggle, pending requests row,
    /// joined chats, more chats, bots, and admin buttons).
    pub fn show_body(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn InfoBackend,
        colors: &dyn Colors,
    ) -> Option<CommunityBoxAction> {
        let mut action = None;
        let Some(details) = backend.community_details(self.community_id) else {
            FlatLabel::plain(
                "Community is unavailable.",
                FlatLabelStyle::box_label(colors),
            )
            .id_salt("community_unavailable")
            .show(ui);
            return None;
        };
        self.title = details.title.clone();

        // Header summary: avatar + title + chat count
        ui.horizontal(|ui| {
            let (av_rect, _) = ui.allocate_exact_size(Vec2::splat(46.0), Sense::hover());
            EmptyUserpic::for_peer(self.community_id.bare(), &details.title, colors).paint_rounded(
                ui.painter(),
                av_rect,
                12.0,
            );
            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.label(
                    egui::RichText::new(&details.title)
                        .font(FontId::new(15.0, family_semibold()))
                        .color(colors.color("windowBoldFg")),
                );
                let count = details.linked_peers.len();
                let status = if count == 1 {
                    "1 chat".to_owned()
                } else {
                    format!("{count} chats")
                };
                ui.label(
                    egui::RichText::new(status)
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

        // Header admin actions (always visible without scrolling past linked chats)
        if details.can_manage_linked_peers || details.can_edit {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if details.can_manage_linked_peers && ui.button("+ Add Chat").clicked() {
                    action = Some(CommunityBoxAction::OpenAddChat(self.community_id));
                }
                if details.can_edit && ui.button("Manage Community").clicked() {
                    action = Some(CommunityBoxAction::OpenManage(self.community_id));
                }
            });
        }

        ui.add_space(10.0);
        divider_bar(ui, 6.0, colors);
        ui.add_space(6.0);

        // "Show as one chat" collapsed-in-dialogs toggle
        let mut collapsed = details.collapsed_in_dialogs;
        if Toggle::new(&mut collapsed, ToggleStyle::default_toggle(colors))
            .with_label("Show as one chat", colors.color("windowFg"))
            .show(ui)
            .changed()
        {
            match backend.toggle_community_collapsed_in_dialogs(self.community_id, collapsed) {
                Ok(()) => {
                    self.error = None;
                    action = Some(CommunityBoxAction::CollapsedToggled(collapsed));
                }
                Err(err) => {
                    self.error = Some(err);
                }
            }
        }
        ui.label(
            egui::RichText::new(
                "Group all chats from this community into a single row in your chat list.",
            )
            .font(FontId::new(12.0, family_regular()))
            .color(colors.color("windowSubTextFg")),
        );

        // Pending link requests banner (admins with manage_linked_peers)
        if details.can_manage_linked_peers && details.pending_requests_count > 0 {
            ui.add_space(8.0);
            let label = format!("Pending Link Requests ({})", details.pending_requests_count);
            if community_action_row(ui, "community_pending_req_row", &label, "Review", colors) {
                action = Some(CommunityBoxAction::OpenPendingRequests(self.community_id));
            }
        }

        // Inline join confirmation banner when user selected an unjoined chat
        if let Some((join_pid, join_title)) = self.pending_join_peer.clone() {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("Join \"{join_title}\"?"))
                        .font(FontId::new(13.0, family_semibold()))
                        .color(colors.color("windowBoldFg")),
                );
                ui.add_space(8.0);
                if ui.button("Join Chat").clicked() {
                    self.pending_join_peer = None;
                    action = Some(CommunityBoxAction::OpenPeer(join_pid));
                }
                if ui.button("Cancel").clicked() {
                    self.pending_join_peer = None;
                }
            });
        }

        // Joined chats section
        let joined = details.joined_chats();
        if !joined.is_empty() {
            ui.add_space(10.0);
            FlatLabel::plain("Your Chats", FlatLabelStyle::subsection_title(colors))
                .id_salt("community_joined_hdr")
                .show(ui);
            ui.add_space(4.0);
            for peer in joined {
                let row_act =
                    community_peer_row(ui, peer, details.can_manage_linked_peers, "joined", colors);
                match row_act {
                    Some(PeerRowClick::Open) => {
                        action = Some(CommunityBoxAction::OpenPeer(peer.peer_id));
                    }
                    Some(PeerRowClick::Remove) => {
                        match backend.toggle_community_peer_link(
                            self.community_id,
                            peer.peer_id,
                            CommunityPeerLinkAction::Deleted,
                        ) {
                            Ok(()) => {
                                action = Some(CommunityBoxAction::PeerRemoved(peer.peer_id));
                            }
                            Err(err) => self.error = Some(err),
                        }
                    }
                    None => {}
                }
            }
        }

        // Discoverable chats section
        let discoverable = details.discoverable_chats();
        if !discoverable.is_empty() {
            ui.add_space(10.0);
            FlatLabel::plain("More Chats", FlatLabelStyle::subsection_title(colors))
                .id_salt("community_more_hdr")
                .show(ui);
            ui.add_space(4.0);
            for peer in discoverable {
                let row_act =
                    community_peer_row(ui, peer, details.can_manage_linked_peers, "more", colors);
                match row_act {
                    Some(PeerRowClick::Open) => {
                        self.pending_join_peer = Some((peer.peer_id, peer.title.clone()));
                    }
                    Some(PeerRowClick::Remove) => {
                        match backend.toggle_community_peer_link(
                            self.community_id,
                            peer.peer_id,
                            CommunityPeerLinkAction::Deleted,
                        ) {
                            Ok(()) => {
                                action = Some(CommunityBoxAction::PeerRemoved(peer.peer_id));
                            }
                            Err(err) => self.error = Some(err),
                        }
                    }
                    None => {}
                }
            }
        }

        // Bots section
        let bots = details.linked_bots();
        if !bots.is_empty() {
            ui.add_space(10.0);
            FlatLabel::plain("Bots", FlatLabelStyle::subsection_title(colors))
                .id_salt("community_bots_hdr")
                .show(ui);
            ui.add_space(4.0);
            for bot in bots {
                let row_act =
                    community_peer_row(ui, bot, details.can_manage_linked_peers, "bot", colors);
                match row_act {
                    Some(PeerRowClick::Open) => {
                        if bot.is_hidden() {
                            action = Some(CommunityBoxAction::Toast(
                                "This bot is hidden and not accessible.".to_owned(),
                            ));
                        } else {
                            action = Some(CommunityBoxAction::OpenPeer(bot.peer_id));
                        }
                    }
                    Some(PeerRowClick::Remove) => {
                        match backend.toggle_community_peer_link(
                            self.community_id,
                            bot.peer_id,
                            CommunityPeerLinkAction::Deleted,
                        ) {
                            Ok(()) => {
                                action = Some(CommunityBoxAction::PeerRemoved(bot.peer_id));
                            }
                            Err(err) => self.error = Some(err),
                        }
                    }
                    None => {}
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

        action
    }
}

/// Who can add chats into a community (`ManageCommunityBox` radio setting).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommunityAddPolicy {
    /// Any member can submit/add chats (`default_banned_rights` lacks `MANAGE_LINKED_PEERS`).
    AllMembers,
    /// Only administrators can add chats (`default_banned_rights` sets `MANAGE_LINKED_PEERS`).
    OnlyAdmins,
}

/// Sub-actions emitted from [`ManageCommunityBoxState::show_body`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManageCommunityAction {
    /// Open Administrators list for this community.
    OpenAdministrators(PeerId),
    /// Open Pending Link Requests box.
    OpenPendingRequests(PeerId),
    /// Open Add Chat to Community box.
    OpenAddChat(PeerId),
    /// Community was deleted.
    Deleted(PeerId),
    /// Peer link visibility or membership changed.
    PeerLinksChanged,
    /// Show toast message.
    Toast(String),
}

/// Manage Community settings modal (`boxes/peers/manage_community_box.cpp`).
#[derive(Debug, Clone)]
pub struct ManageCommunityBoxState {
    /// Community peer ID.
    pub community_id: PeerId,
    /// Editable community title.
    pub title: String,
    /// Who can add chats radio policy.
    pub add_policy: CommunityAddPolicy,
    /// Whether current user can edit title & policy.
    pub can_edit: bool,
    /// Whether current user can delete community.
    pub can_delete: bool,
    /// Confirming delete step.
    pub confirm_delete: bool,
    /// Optional validation / RPC error.
    pub error: Option<String>,
}

impl ManageCommunityBoxState {
    /// Creates a [`ManageCommunityBoxState`] initialized from `details`.
    #[must_use]
    pub fn from_details(details: &CommunityDetails) -> Self {
        Self {
            community_id: details.id,
            title: details.title.clone(),
            add_policy: if details.anyone_can_add_peers {
                CommunityAddPolicy::AllMembers
            } else {
                CommunityAddPolicy::OnlyAdmins
            },
            can_edit: details.can_edit,
            can_delete: details.can_delete,
            confirm_delete: false,
            error: None,
        }
    }

    /// Saves updated title and who-can-add policy via `backend`.
    pub fn submit(&mut self, backend: &mut dyn InfoBackend) -> Result<(), String> {
        let trimmed = self.title.trim();
        if trimmed.is_empty() {
            let msg = "Community name cannot be empty.".to_owned();
            self.error = Some(msg.clone());
            return Err(msg);
        }
        let only_admins = self.add_policy == CommunityAddPolicy::OnlyAdmins;
        match backend.update_community_settings(self.community_id, trimmed, only_admins) {
            Ok(()) => {
                self.error = None;
                Ok(())
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
        let modal = BoxModal::new(id, "Manage Community").width(st::box_wide_width.0 as f32 + 24.0);
        if self.can_edit {
            modal
                .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
                .add_button(BoxButton::default_btn("save", tr::lng_settings_save()))
        } else {
            modal.add_button(BoxButton::default_btn("close", tr::lng_close()))
        }
    }

    /// Renders the Manage Community editor body.
    pub fn show_body(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn InfoBackend,
        colors: &dyn Colors,
    ) -> Option<ManageCommunityAction> {
        let mut action = None;
        let input_st = InputFieldStyle::default_input(colors);

        // 1. Title input
        if self.can_edit {
            let title_err = self.error.is_some() && self.title.trim().is_empty();
            InputField::single_line(
                "manage_community_title",
                &mut self.title,
                "Community Name",
                input_st,
            )
            .max_length(MAX_GROUP_CHANNEL_TITLE)
            .error(title_err)
            .show(ui);
        } else {
            ui.label(
                egui::RichText::new(&self.title)
                    .font(FontId::new(15.0, family_semibold()))
                    .color(colors.color("windowBoldFg")),
            );
        }

        ui.add_space(10.0);
        divider_bar(ui, 6.0, colors);
        ui.add_space(6.0);

        // 2. "Who can add chats" radio group
        FlatLabel::plain(
            "Who Can Add Chats",
            FlatLabelStyle::subsection_title(colors),
        )
        .id_salt("community_who_add_hdr")
        .show(ui);
        ui.add_space(4.0);

        let cb_st = CheckboxStyle::box_checkbox(colors);
        let rad_st = RadioStyle::default_radio(colors);
        ui.add_enabled_ui(self.can_edit, |ui| {
            Radio::new(
                &mut self.add_policy,
                CommunityAddPolicy::AllMembers,
                "All members",
                cb_st,
                rad_st,
            )
            .show(ui);
            ui.label(
                egui::RichText::new("Members can request adding their chats to this community.")
                    .font(FontId::new(12.0, family_regular()))
                    .color(colors.color("windowSubTextFg")),
            );
            ui.add_space(6.0);
            Radio::new(
                &mut self.add_policy,
                CommunityAddPolicy::OnlyAdmins,
                "Only administrators",
                cb_st,
                rad_st,
            )
            .show(ui);
            ui.label(
                egui::RichText::new("Only community administrators can link chats.")
                    .font(FontId::new(12.0, family_regular()))
                    .color(colors.color("windowSubTextFg")),
            );
        });

        ui.add_space(10.0);
        divider_bar(ui, 6.0, colors);
        ui.add_space(6.0);

        // 3. Management rows: Administrators, Pending Requests, Removed Users
        if let Some(details) = backend.community_details(self.community_id) {
            let admins_badge = if details.admins_count > 0 {
                details.admins_count.to_string()
            } else {
                String::new()
            };
            if community_action_row(
                ui,
                "manage_comm_admins",
                &tr::lng_manage_peer_administrators(),
                &admins_badge,
                colors,
            ) {
                action = Some(ManageCommunityAction::OpenAdministrators(self.community_id));
            }

            if details.can_manage_linked_peers && details.pending_requests_count > 0 {
                let req_badge = details.pending_requests_count.to_string();
                if community_action_row(
                    ui,
                    "manage_comm_requests",
                    "Pending Link Requests",
                    &req_badge,
                    colors,
                ) {
                    action = Some(ManageCommunityAction::OpenPendingRequests(
                        self.community_id,
                    ));
                }
            }

            let kicked_badge = if details.kicked_count > 0 {
                details.kicked_count.to_string()
            } else {
                String::new()
            };
            if community_action_row(
                ui,
                "manage_comm_removed",
                "Removed Users",
                &kicked_badge,
                colors,
            ) {
                action = Some(ManageCommunityAction::Toast(format!(
                    "{} removed participant(s)",
                    details.kicked_count
                )));
            }

            // 4. Linked chats management
            if details.can_manage_linked_peers {
                ui.add_space(8.0);
                divider_bar(ui, 6.0, colors);
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    FlatLabel::plain("Linked Chats", FlatLabelStyle::subsection_title(colors))
                        .id_salt("manage_comm_linked_hdr")
                        .show(ui);
                    ui.add_space(8.0);
                    if ui.button("+ Add Chat").clicked() {
                        action = Some(ManageCommunityAction::OpenAddChat(self.community_id));
                    }
                });
                ui.add_space(4.0);
                for peer in &details.linked_peers {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(&peer.title)
                                .font(FontId::new(13.5, family_semibold()))
                                .color(colors.color("windowBoldFg")),
                        );
                        let vis_label = if peer.is_hidden() {
                            "Hidden"
                        } else {
                            "Visible"
                        };
                        if ui
                            .small_button(vis_label)
                            .on_hover_text("Toggle visible / hidden in community")
                            .clicked()
                        {
                            let next = if peer.is_hidden() {
                                CommunityPeerLinkAction::Visible
                            } else {
                                CommunityPeerLinkAction::Hidden
                            };
                            match backend.toggle_community_peer_link(
                                self.community_id,
                                peer.peer_id,
                                next,
                            ) {
                                Ok(()) => action = Some(ManageCommunityAction::PeerLinksChanged),
                                Err(err) => self.error = Some(err),
                            }
                        }
                        if ui.small_button("Remove").clicked() {
                            match backend.toggle_community_peer_link(
                                self.community_id,
                                peer.peer_id,
                                CommunityPeerLinkAction::Deleted,
                            ) {
                                Ok(()) => action = Some(ManageCommunityAction::PeerLinksChanged),
                                Err(err) => self.error = Some(err),
                            }
                        }
                    });
                }
            }
        }

        // 5. Delete community attention action
        if self.can_delete {
            ui.add_space(10.0);
            divider_bar(ui, 6.0, colors);
            ui.add_space(6.0);
            if !self.confirm_delete {
                let btn = egui::Button::new(
                    egui::RichText::new("Delete Community")
                        .color(colors.color("attentionButtonFg")),
                );
                if ui.add(btn).clicked() {
                    self.confirm_delete = true;
                }
            } else {
                ui.label(
                    egui::RichText::new("Delete this community? Member chats will remain intact.")
                        .font(FontId::new(12.5, family_regular()))
                        .color(colors.color("attentionButtonFg")),
                );
                ui.horizontal(|ui| {
                    if ui.button("Confirm Delete").clicked() {
                        match backend.delete_community(self.community_id) {
                            Ok(()) => {
                                self.confirm_delete = false;
                                action = Some(ManageCommunityAction::Deleted(self.community_id));
                            }
                            Err(err) => self.error = Some(err),
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        self.confirm_delete = false;
                    }
                });
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

        action
    }
}

/// Create Community dialog (`communities.create`).
#[derive(Debug, Clone)]
pub struct CreateCommunityBoxState {
    /// Initial peer to attach to the new community.
    pub initial_peer_id: PeerId,
    /// Display title of the initial peer.
    pub initial_peer_title: String,
    /// New community title.
    pub title: String,
    /// Optional community about text.
    pub about: String,
    /// Whether the initial peer is linked as hidden.
    pub hidden: bool,
    /// Optional validation/RPC error.
    pub error: Option<String>,
}

impl CreateCommunityBoxState {
    /// Creates a new [`CreateCommunityBoxState`] anchored on `initial_peer_id`.
    #[must_use]
    pub fn new(initial_peer_id: PeerId, initial_peer_title: impl Into<String>) -> Self {
        Self {
            initial_peer_id,
            initial_peer_title: initial_peer_title.into(),
            title: String::new(),
            about: String::new(),
            hidden: false,
            error: None,
        }
    }

    /// Submits `communities.create` through `backend`.
    pub fn submit(&mut self, backend: &mut dyn InfoBackend) -> Result<PeerId, String> {
        let title = self.title.trim().to_owned();
        if title.is_empty() {
            let msg = "Please enter a community name.".to_owned();
            self.error = Some(msg.clone());
            return Err(msg);
        }
        let req = CreateCommunityRequest {
            title,
            about: self.about.trim().to_owned(),
            initial_peer_id: self.initial_peer_id,
            hidden: self.hidden,
        };
        match backend.create_community(req) {
            Ok(cid) => {
                self.error = None;
                Ok(cid)
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
        BoxModal::new(id, "New Community")
            .width(st::box_width.0 as f32 + 36.0)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::default_btn(
                "create",
                tr::lng_create_group_create(),
            ))
    }

    /// Renders the Create Community form.
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) {
        let input_st = InputFieldStyle::default_input(colors);
        ui.label(
            egui::RichText::new(format!("Initial chat: {}", self.initial_peer_title))
                .font(FontId::new(13.0, family_regular()))
                .color(colors.color("windowSubTextFg")),
        );
        ui.add_space(6.0);
        let title_err = self.error.is_some() && self.title.trim().is_empty();
        InputField::single_line(
            "create_community_title",
            &mut self.title,
            "Community Name",
            input_st,
        )
        .max_length(MAX_GROUP_CHANNEL_TITLE)
        .error(title_err)
        .show(ui);
        ui.add_space(8.0);
        InputField::multi_line(
            "create_community_about",
            &mut self.about,
            "Description (optional)",
            input_st,
        )
        .max_length(MAX_CHANNEL_DESCRIPTION)
        .show(ui);
        ui.add_space(8.0);
        Toggle::new(&mut self.hidden, ToggleStyle::default_toggle(colors))
            .with_label(
                "Hide initial chat from non-members",
                colors.color("windowFg"),
            )
            .show(ui);

        if let Some(ref err) = self.error {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(err)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("boxTextFgError")),
            );
        }
    }
}

/// Actions emitted from [`CommunityPendingRequestsBoxState::show_body`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommunityRequestsBoxAction {
    /// A single link request was approved or rejected.
    SingleResolved {
        /// Peer ID of the request.
        peer_id: PeerId,
        /// True if approved, false if rejected.
        approved: bool,
    },
    /// All pending requests were approved or rejected.
    AllResolved {
        /// True if approved, false if rejected.
        approved: bool,
    },
}

/// Pending community peer link requests approval dialog
/// (`boxes/peers/community_pending_requests_box.cpp`).
#[derive(Debug, Clone)]
pub struct CommunityPendingRequestsBoxState {
    /// Community peer ID.
    pub community_id: PeerId,
    /// Loaded requests slice.
    pub slice: CommunityPeerRequestsSlice,
    /// Whether initial load happened.
    pub loaded: bool,
    /// Optional error message.
    pub error: Option<String>,
}

impl CommunityPendingRequestsBoxState {
    /// Creates a new [`CommunityPendingRequestsBoxState`].
    #[must_use]
    pub fn new(community_id: PeerId) -> Self {
        Self {
            community_id,
            slice: CommunityPeerRequestsSlice::default(),
            loaded: false,
            error: None,
        }
    }

    /// Reloads pending peer link requests from `backend`.
    pub fn reload(&mut self, backend: &dyn InfoBackend) {
        match backend.community_peer_link_requests(self.community_id, "", 50) {
            Ok(slice) => {
                self.slice = slice;
                self.loaded = true;
                self.error = None;
            }
            Err(err) => {
                self.loaded = true;
                self.error = Some(err);
            }
        }
    }

    /// Approves (`reject = false`) or rejects (`reject = true`) a single peer link request.
    pub fn decide_one(
        &mut self,
        peer_id: PeerId,
        reject: bool,
        backend: &mut dyn InfoBackend,
    ) -> Result<(), String> {
        backend.toggle_community_peer_link_request_approval(self.community_id, peer_id, reject)?;
        self.slice.list.retain(|r| r.peer_id != peer_id);
        self.slice.total_count = (self.slice.total_count - 1).max(0);
        self.error = None;
        Ok(())
    }

    /// Approves or rejects all pending peer link requests.
    pub fn decide_all(
        &mut self,
        reject: bool,
        backend: &mut dyn InfoBackend,
    ) -> Result<(), String> {
        backend.toggle_all_community_peer_link_request_approval(self.community_id, reject)?;
        self.slice.list.clear();
        self.slice.total_count = 0;
        self.error = None;
        Ok(())
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        let mut modal = BoxModal::new(id, "Pending Link Requests")
            .width(st::box_wide_width.0 as f32 + 24.0)
            .add_button(BoxButton::default_btn("close", tr::lng_close()));
        if !self.slice.list.is_empty() {
            modal = modal
                .add_button(BoxButton::attention("reject_all", "Reject All"))
                .add_button(BoxButton::default_btn("approve_all", "Approve All"));
        }
        modal
    }

    /// Renders the pending requests list with per-row Approve / Reject buttons.
    pub fn show_body(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn InfoBackend,
        colors: &dyn Colors,
    ) -> Option<CommunityRequestsBoxAction> {
        if !self.loaded {
            self.reload(backend);
        }
        let mut action = None;

        if self.slice.list.is_empty() {
            FlatLabel::plain(
                "No pending link requests.",
                FlatLabelStyle::box_label(colors),
            )
            .id_salt("comm_reqs_empty")
            .show(ui);
        } else {
            let items = self.slice.list.clone();
            for req in items {
                ui.horizontal(|ui| {
                    let (av_rect, _) = ui.allocate_exact_size(Vec2::splat(36.0), Sense::hover());
                    EmptyUserpic::for_peer(req.peer_id.bare(), &req.peer_title, colors)
                        .paint_circle(ui.painter(), av_rect);
                    ui.add_space(8.0);
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new(&req.peer_title)
                                .font(FontId::new(13.5, family_semibold()))
                                .color(colors.color("windowBoldFg")),
                        );
                        let vis = if req.visible { "Visible" } else { "Hidden" };
                        let sub = if req.requested_by_name.is_empty() {
                            vis.to_owned()
                        } else {
                            format!("Requested by {} · {vis}", req.requested_by_name)
                        };
                        ui.label(
                            egui::RichText::new(sub)
                                .font(FontId::new(12.0, family_regular()))
                                .color(colors.color("windowSubTextFg")),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Reject").clicked() {
                            match self.decide_one(req.peer_id, true, backend) {
                                Ok(()) => {
                                    action = Some(CommunityRequestsBoxAction::SingleResolved {
                                        peer_id: req.peer_id,
                                        approved: false,
                                    });
                                }
                                Err(err) => self.error = Some(err),
                            }
                        }
                        if ui.small_button("Approve").clicked() {
                            match self.decide_one(req.peer_id, false, backend) {
                                Ok(()) => {
                                    action = Some(CommunityRequestsBoxAction::SingleResolved {
                                        peer_id: req.peer_id,
                                        approved: true,
                                    });
                                }
                                Err(err) => self.error = Some(err),
                            }
                        }
                    });
                });
                ui.add_space(6.0);
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

        action
    }
}

/// Add a chat, channel, or bot to a community (`boxes/peers/add_to_community_box.cpp`).
#[derive(Debug, Clone)]
pub struct AddToCommunityBoxState {
    /// Target community peer ID.
    pub community_id: PeerId,
    /// Selected peer to link.
    pub selected_peer: Option<PeerId>,
    /// True = visible to all community members; false = hidden.
    pub visible: bool,
    /// Search filter query.
    pub query: String,
    /// Optional status/error message.
    pub error: Option<String>,
}

impl AddToCommunityBoxState {
    /// Creates a new [`AddToCommunityBoxState`].
    #[must_use]
    pub fn new(community_id: PeerId) -> Self {
        Self {
            community_id,
            selected_peer: None,
            visible: true,
            query: String::new(),
            error: None,
        }
    }

    /// Submits `communities.togglePeerLink` (`Visible` or `Hidden`) for the selected peer.
    /// Returns `Ok(true)` if linked immediately, `Ok(false)` if a request was created
    /// (`COMMUNITY_REQUEST_CREATED`), or `Err(msg)` on failure.
    pub fn submit(&mut self, backend: &mut dyn InfoBackend) -> Result<bool, String> {
        let Some(peer_id) = self.selected_peer else {
            let msg = "Please select a chat to add.".to_owned();
            self.error = Some(msg.clone());
            return Err(msg);
        };
        let action = if self.visible {
            CommunityPeerLinkAction::Visible
        } else {
            CommunityPeerLinkAction::Hidden
        };
        match backend.toggle_community_peer_link(self.community_id, peer_id, action) {
            Ok(()) => {
                self.error = None;
                Ok(true)
            }
            Err(err) if err.contains(COMMUNITY_REQUEST_CREATED) => {
                self.error = None;
                Ok(false)
            }
            Err(err) if err.contains(COMMUNITY_PEERS_TOO_MUCH) => {
                let msg = format!(
                    "This community already has the maximum of {DEFAULT_COMMUNITY_PEERS_LIMIT} linked chats."
                );
                self.error = Some(msg.clone());
                Err(msg)
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
        BoxModal::new(id, "Add Chat to Community")
            .width(st::box_width.0 as f32 + 40.0)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::default_btn("add", "Add Chat"))
    }

    /// Renders the peer chooser and visibility toggle.
    pub fn show_body(
        &mut self,
        ui: &mut Ui,
        all_peers: &[crate::info::backend::PeerProfile],
        backend: &dyn InfoBackend,
        colors: &dyn Colors,
    ) {
        let input_st = InputFieldStyle::default_input(colors);
        InputField::single_line(
            "add_to_comm_search",
            &mut self.query,
            "Search groups, channels, or bots...",
            input_st,
        )
        .show(ui);
        ui.add_space(6.0);

        Toggle::new(&mut self.visible, ToggleStyle::default_toggle(colors))
            .with_label("Visible to all community members", colors.color("windowFg"))
            .show(ui);
        ui.add_space(6.0);

        let existing: Vec<PeerId> = backend
            .community_details(self.community_id)
            .map(|d| d.linked_peers.iter().map(|p| p.peer_id).collect())
            .unwrap_or_default();
        let q = self.query.trim().to_lowercase();

        for peer in all_peers {
            if peer.id == self.community_id || existing.contains(&peer.id) {
                continue;
            }
            if !peer.kind.is_group()
                && !peer.kind.is_channel()
                && peer.kind != crate::info::backend::InfoPeerKind::Bot
            {
                continue;
            }
            if !q.is_empty() && !peer.title.to_lowercase().contains(&q) {
                continue;
            }
            let selected = self.selected_peer == Some(peer.id);
            let resp = ui.selectable_label(selected, &peer.title);
            if resp.clicked() {
                self.selected_peer = Some(peer.id);
                self.error = None;
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
    }
}

enum PeerRowClick {
    Open,
    Remove,
}

fn community_peer_row(
    ui: &mut Ui,
    peer: &CommunityLinkedPeer,
    can_remove: bool,
    salt: &str,
    colors: &dyn Colors,
) -> Option<PeerRowClick> {
    let mut result = None;
    ui.horizontal(|ui| {
        let row_w = (ui.available_width() - if can_remove { 64.0 } else { 0.0 }).max(120.0);
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(row_w, 46.0), Sense::click());
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter()
                .rect_filled(rect, CornerRadius::same(6), colors.color("windowBgOver"));
        }
        let av_rect = Rect::from_min_size(
            Pos2::new(rect.left() + 4.0, rect.center().y - 18.0),
            Vec2::splat(36.0),
        );
        EmptyUserpic::for_peer(peer.peer_id.bare(), &peer.title, colors)
            .paint_circle(ui.painter(), av_rect);

        let tx = av_rect.right() + 10.0;
        ui.painter().text(
            Pos2::new(tx, rect.top() + 14.0),
            Align2::LEFT_CENTER,
            &peer.title,
            FontId::new(13.5, family_semibold()),
            colors.color("windowBoldFg"),
        );
        let sub = if peer.is_bot {
            if peer.is_hidden() {
                "bot · hidden".to_owned()
            } else {
                "bot".to_owned()
            }
        } else if peer.members_count > 0 {
            let noun = if peer.is_channel {
                "subscribers"
            } else {
                "members"
            };
            format!("{} {noun}", peer.members_count)
        } else if !peer.last_message.is_empty() {
            peer.last_message.clone()
        } else if peer.is_channel {
            "Channel".to_owned()
        } else {
            "Group".to_owned()
        };
        ui.painter().text(
            Pos2::new(tx, rect.top() + 32.0),
            Align2::LEFT_CENTER,
            &sub,
            FontId::new(12.0, family_regular()),
            colors.color("windowSubTextFg"),
        );

        if peer.unread_count > 0 {
            let badge = peer.unread_count.to_string();
            let badge_bg = if peer.muted {
                colors.color("dialogsUnreadBgMuted")
            } else {
                colors.color("dialogsUnreadBg")
            };
            let badge_rect = Rect::from_min_size(
                Pos2::new(rect.right() - 28.0, rect.center().y - 10.0),
                Vec2::new(24.0, 20.0),
            );
            ui.painter()
                .rect_filled(badge_rect, CornerRadius::same(10), badge_bg);
            ui.painter().text(
                badge_rect.center(),
                Align2::CENTER_CENTER,
                &badge,
                FontId::new(11.0, family_semibold()),
                colors.color("dialogsUnreadFg"),
            );
        }

        if resp.clicked() {
            result = Some(PeerRowClick::Open);
        }
        if can_remove {
            let rm_id = ui.make_persistent_id((salt, "rm", peer.peer_id.0));
            ui.push_id(rm_id, |ui| {
                if ui.small_button("Remove").clicked() {
                    result = Some(PeerRowClick::Remove);
                }
            });
        }
    });
    result
}

fn community_action_row(
    ui: &mut Ui,
    id_salt: &str,
    label: &str,
    badge: &str,
    colors: &dyn Colors,
) -> bool {
    let w = ui.available_width().max(160.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 38.0), Sense::click());
    let _ = ui.make_persistent_id(id_salt);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), colors.color("windowBgOver"));
    }
    ui.painter().text(
        Pos2::new(rect.left() + 8.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::new(13.5, family_regular()),
        colors.color("windowFg"),
    );
    if !badge.is_empty() {
        ui.painter().text(
            Pos2::new(rect.right() - 8.0, rect.center().y),
            Align2::RIGHT_CENTER,
            badge,
            FontId::new(13.0, family_semibold()),
            colors.color("windowActiveTextFg"),
        );
    }
    resp.clicked()
}
