//! Shareable Chat Folder Multi-Link Manager dialog (`EditFilterLinksBox`).
//!
//! Port origin: `Telegram/SourceFiles/boxes/filters/edit_filter_links.cpp`
//! and `Telegram/SourceFiles/boxes/filters/edit_filter_links.h`.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, Id, Key, Pos2, Rect, ScrollArea, Sense, Stroke, Ui,
    Vec2,
};
use tg_lang::tr;
use tg_ui::fonts::font_id_from_style;
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{Colors, InputFieldStyle, RoundButtonStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::buttons::RoundButton;
use tg_ui::widgets::input_field::InputField;

use crate::settings::backend::{
    FolderInviteLink, MAX_FOLDER_LINK_TITLE_LEN, PeerRef, SettingsBackend,
};

/// Active subview inside [`FolderLinksBoxState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderLinksView {
    /// Main list of exported invite links + optional joined-folder updates section.
    List,
    /// Create a brand-new shareable invite link (`chatlists.exportChatlistInvite`).
    Create {
        /// Optional title (`<= 32` chars).
        title: String,
        /// Selected peer IDs to include in the new link.
        selected_peers: Vec<i64>,
    },
    /// Edit an existing shareable invite link (`chatlists.editExportedInvite`).
    Edit {
        /// Existing invite slug.
        slug: String,
        /// Full invite URL.
        url: String,
        /// Editable link label (`<= 32` chars).
        title: String,
        /// Selected peer IDs in this link.
        selected_peers: Vec<i64>,
    },
}

/// High-level action emitted by [`FolderLinksBoxState::show_modal`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderLinksAction {
    /// Close the multi-link manager dialog.
    Close,
    /// Show a status toast message.
    Toast(String),
}

/// Stateful Multi-Link Manager for a chat folder (`boxes/filters/edit_filter_links.cpp`).
#[derive(Debug, Clone)]
pub struct FolderLinksBoxState {
    /// Target dialog filter ID.
    pub filter_id: i32,
    /// Folder title displayed in the header.
    pub folder_title: String,
    /// Whether this folder is a joined/shared chatlist that supports checking updates.
    pub shareable: bool,
    /// Default peers included in the folder (pre-selected when creating a new link).
    pub folder_peers: Vec<i64>,
    /// Loaded exported links for this folder.
    pub links: Vec<FolderInviteLink>,
    /// Whether `folder_invite_links` has been loaded at least once.
    pub loaded: bool,
    /// Active view (`List`, `Create`, or `Edit`).
    pub view: FolderLinksView,
    /// Slug awaiting delete confirmation.
    pub confirm_delete_slug: Option<String>,
    /// Pending new peers fetched via `chatlists.getChatlistUpdates`.
    pub pending_update_peers: Option<Vec<i64>>,
    /// Peers currently checked for `chatlists.joinChatlistUpdates`.
    pub selected_update_peers: Vec<i64>,
    /// Inline validation or RPC error message.
    pub error: Option<String>,
    /// Suppresses the in-frame `Key::Escape` check when `WindowController` already handled it.
    skip_escape_frame: bool,
}

impl FolderLinksBoxState {
    /// Creates a new folder multi-link manager dialog state for `filter_id`.
    #[must_use]
    pub fn new(
        filter_id: i32,
        folder_title: impl Into<String>,
        shareable: bool,
        folder_peers: Vec<i64>,
    ) -> Self {
        Self {
            filter_id,
            folder_title: folder_title.into(),
            shareable,
            folder_peers,
            links: Vec::new(),
            loaded: false,
            view: FolderLinksView::List,
            confirm_delete_slug: None,
            pending_update_peers: None,
            selected_update_peers: Vec::new(),
            error: None,
            skip_escape_frame: false,
        }
    }

    /// Handles `Escape` from an outer controller (`SettingsController::pop_section`).
    /// Returns `true` if handled internally (subview -> `List`), or `false` if the modal should close.
    pub fn handle_escape(&mut self) -> bool {
        if !matches!(self.view, FolderLinksView::List) {
            self.view = FolderLinksView::List;
            self.error = None;
            self.skip_escape_frame = true;
            true
        } else {
            false
        }
    }

    /// Builds a `tg-ui` [`BoxModal`] descriptor for this dialog.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        BoxModal::new(id, format!("Invite Links — {}", self.folder_title))
            .width(440.0)
            .add_button(BoxButton::default_btn("close", tr::lng_close()))
    }

    /// Reloads links from `backend.folder_invite_links(self.filter_id)`.
    pub fn refresh(&mut self, backend: &dyn SettingsBackend) {
        self.links = backend.folder_invite_links(self.filter_id);
        self.loaded = true;
    }

    /// Renders the dialog as a centered modal overlay (called from `settings/folders.rs`).
    pub fn show_modal(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn SettingsBackend,
        colors: &dyn Colors,
    ) -> Option<FolderLinksAction> {
        if !self.loaded {
            self.refresh(backend);
        }

        let mut action = None;
        let screen = ui.ctx().viewport_rect();

        // Dimmed backdrop.
        egui::Area::new(Id::unique(("tg_folder_links_backdrop", self.filter_id)))
            .order(egui::Order::Middle)
            .fixed_pos(screen.min)
            .show(ui.ctx(), |ui| {
                let (r, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
                ui.painter().rect_filled(
                    r,
                    CornerRadius::ZERO,
                    Color32::from_rgba_unmultiplied(0, 0, 0, 120),
                );
                if resp.clicked() {
                    action = Some(FolderLinksAction::Close);
                }
            });

        egui::Area::new(Id::unique(("tg_folder_links_modal", self.filter_id)))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                egui::Frame::NONE
                    .fill(colors.color("boxBg"))
                    .stroke(Stroke::new(1.0, colors.color("shadowFg")))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(egui::Margin::same(18))
                    .show(ui, |ui| {
                        ui.set_width(430.0);
                        ui.set_max_height((screen.height() - 80.0).max(320.0));
                        if let Some(ev) = self.show_body(ui, backend, colors) {
                            action = Some(ev);
                        }
                    });
            });

        if self.skip_escape_frame {
            self.skip_escape_frame = false;
        } else if ui.ctx().input(|i| i.key_pressed(Key::Escape)) {
            if !matches!(self.view, FolderLinksView::List) {
                self.view = FolderLinksView::List;
                self.error = None;
            } else {
                action = Some(FolderLinksAction::Close);
            }
        }

        action
    }

    /// Renders the dialog body (links list, create/edit subview, and joined-folder updates).
    pub fn show_body(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn SettingsBackend,
        colors: &dyn Colors,
    ) -> Option<FolderLinksAction> {
        if !self.loaded {
            self.refresh(backend);
        }
        let contacts = backend.contacts();
        let mut action = None;

        // Header
        ui.horizontal(|ui| {
            let heading = match &self.view {
                FolderLinksView::List => format!("Invite Links — {}", self.folder_title),
                FolderLinksView::Create { .. } => "New Invite Link".to_owned(),
                FolderLinksView::Edit { .. } => "Edit Invite Link".to_owned(),
            };
            ui.label(
                egui::RichText::new(heading)
                    .font(font_id_from_style(st::box_title_font))
                    .color(colors.color("windowBoldFg")),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("×").clicked() {
                    action = Some(FolderLinksAction::Close);
                }
                if matches!(self.view, FolderLinksView::List)
                    && ui.small_button("Refresh").clicked()
                {
                    self.refresh(backend);
                }
            });
        });
        ui.add_space(6.0);

        match self.view.clone() {
            FolderLinksView::List => {
                if let Some(ev) = self.show_list_view(ui, &contacts, backend, colors) {
                    action = Some(ev);
                }
            }
            FolderLinksView::Create {
                mut title,
                mut selected_peers,
            } => {
                let outcome = self.show_editor_form(
                    ui,
                    None,
                    &mut title,
                    &mut selected_peers,
                    &contacts,
                    colors,
                );
                match outcome {
                    EditorOutcome::None => {
                        self.view = FolderLinksView::Create {
                            title,
                            selected_peers,
                        };
                    }
                    EditorOutcome::Cancel => {
                        self.view = FolderLinksView::List;
                        self.error = None;
                    }
                    EditorOutcome::Save => {
                        let clean_title: String = title
                            .trim()
                            .chars()
                            .take(MAX_FOLDER_LINK_TITLE_LEN)
                            .collect();
                        if selected_peers.is_empty() {
                            self.error =
                                Some("Select at least one chat to include in the link.".to_owned());
                            self.view = FolderLinksView::Create {
                                title,
                                selected_peers,
                            };
                        } else {
                            match backend.create_folder_invite_link(
                                self.filter_id,
                                &clean_title,
                                &selected_peers,
                            ) {
                                Ok(created) => {
                                    ui.ctx().copy_text(created.url.clone());
                                    self.refresh(backend);
                                    self.view = FolderLinksView::List;
                                    self.error = None;
                                    action = Some(FolderLinksAction::Toast(format!(
                                        "Folder link created & copied: {}",
                                        created.url
                                    )));
                                }
                                Err(err) => {
                                    self.error = Some(err);
                                    self.view = FolderLinksView::Create {
                                        title,
                                        selected_peers,
                                    };
                                }
                            }
                        }
                    }
                }
            }
            FolderLinksView::Edit {
                slug,
                url,
                mut title,
                mut selected_peers,
            } => {
                let outcome = self.show_editor_form(
                    ui,
                    Some(&url),
                    &mut title,
                    &mut selected_peers,
                    &contacts,
                    colors,
                );
                match outcome {
                    EditorOutcome::None => {
                        self.view = FolderLinksView::Edit {
                            slug,
                            url,
                            title,
                            selected_peers,
                        };
                    }
                    EditorOutcome::Cancel => {
                        self.view = FolderLinksView::List;
                        self.error = None;
                    }
                    EditorOutcome::Save => {
                        let clean_title: String = title
                            .trim()
                            .chars()
                            .take(MAX_FOLDER_LINK_TITLE_LEN)
                            .collect();
                        if selected_peers.is_empty() {
                            self.error =
                                Some("Select at least one chat to keep in the link.".to_owned());
                            self.view = FolderLinksView::Edit {
                                slug,
                                url,
                                title,
                                selected_peers,
                            };
                        } else {
                            match backend.edit_folder_invite_link(
                                self.filter_id,
                                &slug,
                                Some(&clean_title),
                                Some(&selected_peers),
                            ) {
                                Ok(_updated) => {
                                    self.refresh(backend);
                                    self.view = FolderLinksView::List;
                                    self.error = None;
                                    action = Some(FolderLinksAction::Toast(
                                        "Folder invite link updated".to_owned(),
                                    ));
                                }
                                Err(err) => {
                                    self.error = Some(err);
                                    self.view = FolderLinksView::Edit {
                                        slug,
                                        url,
                                        title,
                                        selected_peers,
                                    };
                                }
                            }
                        }
                    }
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

    fn show_list_view(
        &mut self,
        ui: &mut Ui,
        contacts: &[PeerRef],
        backend: &mut dyn SettingsBackend,
        colors: &dyn Colors,
    ) -> Option<FolderLinksAction> {
        let mut action = None;

        ui.label(
            egui::RichText::new(
                "Create multiple invite links with different sets of chats to share this folder.",
            )
            .font(font_id_from_style(st::normal_font))
            .color(colors.color("windowSubTextFg")),
        );
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            if RoundButton::new("Create New Link", RoundButtonStyle::default_active(colors))
                .show(ui)
                .clicked()
            {
                let initial_peers = if self.folder_peers.is_empty() {
                    contacts.iter().take(1).map(|p| p.id).collect()
                } else {
                    self.folder_peers.clone()
                };
                self.view = FolderLinksView::Create {
                    title: String::new(),
                    selected_peers: initial_peers,
                };
                self.error = None;
            }
            if self.shareable {
                ui.add_space(8.0);
                if RoundButton::new(
                    "Check for New Chats",
                    RoundButtonStyle::default_light(colors),
                )
                .show(ui)
                .clicked()
                {
                    match backend.check_folder_updates(self.filter_id) {
                        Ok(missing) => {
                            if missing.is_empty() {
                                self.pending_update_peers = None;
                                self.selected_update_peers.clear();
                                action = Some(FolderLinksAction::Toast(
                                    "Shared folder is already up to date.".to_owned(),
                                ));
                            } else {
                                self.selected_update_peers = missing.clone();
                                self.pending_update_peers = Some(missing);
                            }
                            self.error = None;
                        }
                        Err(err) => self.error = Some(err),
                    }
                }
            }
        });

        ui.add_space(10.0);

        // Joined chatlist pending updates panel (`chatlists.getChatlistUpdates` / `joinChatlistUpdates`)
        if let Some(missing) = self.pending_update_peers.clone() {
            egui::Frame::NONE
                .fill(colors.color("windowBgOver"))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "{} new chat(s) available in this shared folder",
                            missing.len()
                        ))
                        .font(font_id_from_style(st::semibold_font))
                        .color(colors.color("windowActiveTextFg")),
                    );
                    ui.add_space(4.0);
                    for pid in &missing {
                        let name = peer_label(*pid, contacts);
                        let mut checked = self.selected_update_peers.contains(pid);
                        if ui.checkbox(&mut checked, name).changed() {
                            if checked {
                                if !self.selected_update_peers.contains(pid) {
                                    self.selected_update_peers.push(*pid);
                                }
                            } else {
                                self.selected_update_peers.retain(|&id| id != *pid);
                            }
                        }
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if RoundButton::new(
                            "Add Selected Chats",
                            RoundButtonStyle::default_active(colors),
                        )
                        .show(ui)
                        .clicked()
                        {
                            let chosen = self.selected_update_peers.clone();
                            match backend.join_folder_updates(self.filter_id, &chosen) {
                                Ok(()) => {
                                    self.pending_update_peers = None;
                                    self.selected_update_peers.clear();
                                    action = Some(FolderLinksAction::Toast(format!(
                                        "Added {} chat(s) from shared folder",
                                        chosen.len()
                                    )));
                                }
                                Err(err) => self.error = Some(err),
                            }
                        }
                        ui.add_space(6.0);
                        if ui.button("Dismiss").clicked() {
                            self.pending_update_peers = None;
                            self.selected_update_peers.clear();
                        }
                    });
                });
            ui.add_space(8.0);
        }

        // Exported invite links list
        if self.links.is_empty() {
            ui.label(
                egui::RichText::new("No invite links created for this folder yet.")
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowSubTextFg")),
            );
        } else {
            let mut edit_link: Option<FolderInviteLink> = None;
            let mut delete_slug: Option<String> = None;
            let mut copy_url: Option<String> = None;
            let links_snapshot = self.links.clone();

            ScrollArea::vertical()
                .id_salt(("tg_folder_links_scroll", self.filter_id))
                .max_height(260.0)
                .show(ui, |ui| {
                    for link in &links_snapshot {
                        egui::Frame::NONE
                            .fill(colors.color("windowBgOver"))
                            .corner_radius(CornerRadius::same(8))
                            .inner_margin(egui::Margin::symmetric(12, 8))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                let display_title = if link.title.trim().is_empty() {
                                    format!("t.me/addlist/{}", link.slug)
                                } else {
                                    link.title.clone()
                                };
                                ui.horizontal(|ui| {
                                    ui.vertical(|ui| {
                                        ui.label(
                                            egui::RichText::new(display_title)
                                                .font(font_id_from_style(st::semibold_font))
                                                .color(colors.color("windowBoldFg")),
                                        );
                                        let chat_word = if link.peers.len() == 1 {
                                            "1 chat".to_owned()
                                        } else {
                                            format!("{} chats", link.peers.len())
                                        };
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "{} · {chat_word}",
                                                link.url
                                            ))
                                            .font(egui::FontId::new(
                                                11.5,
                                                tg_ui::fonts::family_regular(),
                                            ))
                                            .color(colors.color("windowSubTextFg")),
                                        );
                                    });
                                });
                                ui.add_space(6.0);
                                ui.horizontal(|ui| {
                                    if ui.small_button("Copy").clicked() {
                                        copy_url = Some(link.url.clone());
                                    }
                                    if ui.small_button("Edit").clicked() {
                                        edit_link = Some(link.clone());
                                    }
                                    let awaiting = self.confirm_delete_slug.as_deref()
                                        == Some(link.slug.as_str());
                                    let del_lbl =
                                        if awaiting { "Confirm Delete" } else { "Delete" };
                                    if ui
                                        .small_button(
                                            egui::RichText::new(del_lbl)
                                                .color(colors.color("attentionButtonFg")),
                                        )
                                        .clicked()
                                    {
                                        if awaiting {
                                            delete_slug = Some(link.slug.clone());
                                        } else {
                                            self.confirm_delete_slug = Some(link.slug.clone());
                                        }
                                    }
                                });
                            });
                        ui.add_space(6.0);
                    }
                });

            if let Some(url) = copy_url {
                ui.ctx().copy_text(url.clone());
                action = Some(FolderLinksAction::Toast(format!("Copied: {url}")));
            }
            if let Some(link) = edit_link {
                self.confirm_delete_slug = None;
                self.error = None;
                self.view = FolderLinksView::Edit {
                    slug: link.slug,
                    url: link.url,
                    title: link.title,
                    selected_peers: link.peers,
                };
            }
            if let Some(slug) = delete_slug {
                self.confirm_delete_slug = None;
                match backend.delete_folder_invite_link(self.filter_id, &slug) {
                    Ok(_) => {
                        self.refresh(backend);
                        action = Some(FolderLinksAction::Toast(
                            "Folder invite link deleted".to_owned(),
                        ));
                    }
                    Err(err) => self.error = Some(err),
                }
            }
        }

        ui.add_space(10.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if RoundButton::new(&tr::lng_close(), RoundButtonStyle::default_light(colors))
                .show(ui)
                .clicked()
            {
                action = Some(FolderLinksAction::Close);
            }
        });

        action
    }

    fn show_editor_form(
        &mut self,
        ui: &mut Ui,
        existing_url: Option<&str>,
        title: &mut String,
        selected_peers: &mut Vec<i64>,
        contacts: &[PeerRef],
        colors: &dyn Colors,
    ) -> EditorOutcome {
        let mut outcome = EditorOutcome::None;

        if let Some(url) = existing_url {
            ui.label(
                egui::RichText::new(url)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowActiveTextFg")),
            );
            ui.add_space(6.0);
        }

        InputField::single_line(
            "folder_link_title_input",
            title,
            "Link name (optional, up to 32 chars)",
            InputFieldStyle::default_input(colors),
        )
        .max_length(MAX_FOLDER_LINK_TITLE_LEN)
        .show(ui);

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("Included chats ({})", selected_peers.len()))
                    .font(font_id_from_style(st::semibold_font))
                    .color(colors.color("windowActiveTextFg")),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let all_selected =
                    !contacts.is_empty() && contacts.iter().all(|c| selected_peers.contains(&c.id));
                let toggle_lbl = if all_selected {
                    "Deselect All"
                } else {
                    "Select All"
                };
                let (r, resp) = ui.allocate_exact_size(Vec2::new(82.0, 20.0), Sense::click());
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                ui.painter().text(
                    r.center(),
                    Align2::CENTER_CENTER,
                    toggle_lbl,
                    egui::FontId::new(11.5, tg_ui::fonts::family_semibold()),
                    colors.color("windowActiveTextFg"),
                );
                if resp.clicked() {
                    if all_selected {
                        selected_peers.clear();
                    } else {
                        *selected_peers = contacts.iter().map(|c| c.id).collect();
                    }
                }
            });
        });

        ScrollArea::vertical()
            .id_salt(("tg_folder_link_peers", self.filter_id))
            .max_height(180.0)
            .show(ui, |ui| {
                for peer in contacts {
                    let mut checked = selected_peers.contains(&peer.id);
                    let lbl = if peer.username.is_empty() {
                        peer.name.clone()
                    } else {
                        format!("{} (@{})", peer.name, peer.username)
                    };
                    if ui.checkbox(&mut checked, lbl).changed() {
                        if checked {
                            if !selected_peers.contains(&peer.id) {
                                selected_peers.push(peer.id);
                            }
                        } else {
                            selected_peers.retain(|&id| id != peer.id);
                        }
                    }
                }
            });

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let save_lbl = if existing_url.is_some() {
                tr::lng_settings_save()
            } else {
                "Create Link".to_owned()
            };
            if RoundButton::new(&save_lbl, RoundButtonStyle::default_active(colors))
                .show(ui)
                .clicked()
            {
                outcome = EditorOutcome::Save;
            }
            ui.add_space(8.0);
            if RoundButton::new(&tr::lng_cancel(), RoundButtonStyle::default_light(colors))
                .show(ui)
                .clicked()
            {
                outcome = EditorOutcome::Cancel;
            }
        });

        outcome
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorOutcome {
    None,
    Save,
    Cancel,
}

fn peer_label(peer_id: i64, contacts: &[PeerRef]) -> String {
    contacts
        .iter()
        .find(|p| p.id == peer_id)
        .map_or_else(|| format!("Chat #{peer_id}"), |p| p.name.clone())
}

/// Helper to paint a small colored tag dot for folder color index `0..7`.
#[must_use]
pub fn folder_tag_color32(color_index: u8) -> Color32 {
    let hex = crate::settings::backend::FOLDER_TAG_COLORS
        .get(usize::from(color_index % 7))
        .map_or(0x368AD1, |&(_, h, _)| h);
    Color32::from_rgb(
        ((hex >> 16) & 0xFF) as u8,
        ((hex >> 8) & 0xFF) as u8,
        (hex & 0xFF) as u8,
    )
}

/// Paints a small rounded folder color tag swatch tile.
pub fn paint_folder_color_swatch(
    painter: &egui::Painter,
    rect: Rect,
    color: Option<Color32>,
    selected: bool,
    border_fg: Color32,
) {
    let center = rect.center();
    let radius = (rect.width().min(rect.height()) * 0.38).max(6.0);
    if let Some(fill) = color {
        painter.circle_filled(center, radius, fill);
    } else {
        painter.circle_stroke(center, radius, Stroke::new(1.5, border_fg));
        painter.line_segment(
            [
                Pos2::new(center.x - radius * 0.65, center.y + radius * 0.65),
                Pos2::new(center.x + radius * 0.65, center.y - radius * 0.65),
            ],
            Stroke::new(1.5, border_fg),
        );
    }
    if selected {
        painter.circle_stroke(center, radius + 3.0, Stroke::new(2.0, border_fg));
    }
}
