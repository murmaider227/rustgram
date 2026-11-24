//! Edit Username modal box with live validation, availability check, and interactive
//! Collectible Usernames list (`edit_peer_usernames_list`, `channels.{reorder,toggle}Usernames` /
//! `account.{reorder,toggle}Usernames` / `bots.{reorder,toggle}Usernames`).
//!
//! Port origin: `Telegram/SourceFiles/boxes/username_box.h`,
//! `Telegram/SourceFiles/boxes/username_box.cpp`,
//! `Telegram/SourceFiles/boxes/peers/edit_peer_usernames_list.h`,
//! and `Telegram/SourceFiles/boxes/peers/edit_peer_usernames_list.cpp`.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Role, Sense, Ui, Vec2,
    WidgetInfo,
};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold, font_id_from_style};
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{Colors, FlatLabelStyle, InputFieldStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::input_field::InputField;
use tg_ui::widgets::labels::{FlatLabel, divider_bar};

use crate::boxes::strings;
use crate::info::backend::{InfoBackend, PeerProfile, PeerUsername};
use crate::info::values::{
    MAX_USERNAME_LENGTH, UsernameAvailability, format_username_link, normalize_username,
};
use tg_app_core::stand_in::types::PeerId;

/// Interactive actions emitted by the Collectible Usernames list inside [`UsernameBoxState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsernameListAction {
    /// Copied a collectible username link.
    CopiedLink(String),
    /// Activated or deactivated a collectible username (`toggleUsername`).
    Toggled {
        /// Handle without leading `@`.
        username: String,
        /// New active state.
        active: bool,
    },
    /// Reordered active usernames (`reorderUsernames`).
    Reordered(Vec<String>),
}

/// Stateful Edit Username modal box (`UsernameBox` + `UsernamesList`).
#[derive(Debug, Clone)]
pub struct UsernameBoxState {
    /// Target peer id.
    pub peer_id: PeerId,
    /// Original editable username when box opened.
    pub initial_username: String,
    /// Current draft username text.
    pub draft: String,
    /// Latest availability check result.
    pub availability: UsernameAvailability,
    /// Full username list (editable + collectible) for interactive activation & reordering.
    pub all_usernames: Vec<PeerUsername>,
    /// Non-editable collectible usernames attached to the peer (kept in sync with `all_usernames`).
    pub collectibles: Vec<PeerUsername>,
    /// Submission or collectible-list RPC error message.
    pub error: Option<String>,
}

impl UsernameBoxState {
    /// Initializes a username editor from `peer`.
    #[must_use]
    pub fn from_profile(peer: &PeerProfile) -> Self {
        let initial = peer.editable_username().unwrap_or_default().to_owned();
        let all_usernames = peer.usernames.clone();
        let collectibles = peer
            .usernames
            .iter()
            .filter(|u| !u.is_editable)
            .cloned()
            .collect();
        let availability = if initial.is_empty() {
            UsernameAvailability::Empty
        } else {
            UsernameAvailability::Available
        };
        Self {
            peer_id: peer.id,
            initial_username: initial.clone(),
            draft: initial,
            availability,
            all_usernames,
            collectibles,
            error: None,
        }
    }

    /// Re-runs live syntax and backend availability validation on [`Self::draft`].
    pub fn refresh_check(&mut self, backend: &dyn InfoBackend) {
        self.availability = backend.check_username(&self.draft, Some(self.peer_id));
    }

    /// Returns the active usernames in current display order (`UsernamesList::order`).
    #[must_use]
    pub fn active_order(&self) -> Vec<String> {
        self.all_usernames
            .iter()
            .filter(|u| u.is_active)
            .map(|u| u.username.clone())
            .collect()
    }

    /// Syncs `self.all_usernames` and `self.collectibles` from `backend.peer_profile`.
    fn sync_from_backend(&mut self, backend: &dyn InfoBackend) {
        if let Some(peer) = backend.peer_profile(self.peer_id) {
            self.all_usernames = peer.usernames;
            self.collectibles = self
                .all_usernames
                .iter()
                .filter(|u| !u.is_editable)
                .cloned()
                .collect();
        }
    }

    /// Toggles active state for collectible `username` via `backend.toggle_peer_username`.
    pub fn toggle_collectible(
        &mut self,
        username: &str,
        active: bool,
        backend: &mut dyn InfoBackend,
    ) -> Result<(), String> {
        // Persist current order first (matching `edit_peer_usernames_list.cpp:326`).
        let order = self.active_order();
        if order.len() > 1 {
            let _ = backend.reorder_peer_usernames(self.peer_id, &order);
        }
        backend
            .toggle_peer_username(self.peer_id, username, active)
            .inspect_err(|e| {
                self.error = Some(e.clone());
            })?;
        self.error = None;
        self.sync_from_backend(backend);
        Ok(())
    }

    /// Moves an active username at `index` up (`delta = -1`) or down (`delta = 1`)
    /// and invokes `backend.reorder_peer_usernames`.
    pub fn move_active_username(
        &mut self,
        index: usize,
        move_up: bool,
        backend: &mut dyn InfoBackend,
    ) -> Result<Vec<String>, String> {
        let active_count = self.all_usernames.iter().filter(|u| u.is_active).count();
        if index >= active_count || active_count < 2 {
            return Ok(self.active_order());
        }
        let target = if move_up {
            if index == 0 {
                return Ok(self.active_order());
            }
            index - 1
        } else {
            if index + 1 >= active_count {
                return Ok(self.active_order());
            }
            index + 1
        };
        self.all_usernames.swap(index, target);
        let new_order = self.active_order();
        backend
            .reorder_peer_usernames(self.peer_id, &new_order)
            .inspect_err(|e| {
                // Revert local swap on failure.
                self.all_usernames.swap(index, target);
                self.error = Some(e.clone());
            })?;
        self.error = None;
        self.sync_from_backend(backend);
        Ok(new_order)
    }

    /// Saves the updated editable username and active username ordering via `backend`.
    pub fn submit(&mut self, backend: &mut dyn InfoBackend) -> Result<(), String> {
        let clean = normalize_username(&self.draft).to_owned();
        if !clean.is_empty() {
            self.refresh_check(backend);
            if !self.availability.is_valid_public() {
                let msg = self.availability.message(&clean);
                self.error = Some(msg.clone());
                return Err(msg);
            }
        }
        let order = self.active_order();
        if order.len() > 1 {
            let _ = backend.reorder_peer_usernames(self.peer_id, &order);
        }
        match backend.update_username(self.peer_id, &clean) {
            Ok(()) => {
                self.error = None;
                self.sync_from_backend(backend);
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
        BoxModal::new(id, tr::lng_username_title())
            .width(st::box_wide_width.0 as f32)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::default_btn("save", tr::lng_settings_save()))
    }

    /// Renders the `@username` input, live availability feedback, `t.me/` preview,
    /// and interactive Collectible Usernames list (`edit_peer_usernames_list.cpp`).
    pub fn show_body(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn InfoBackend,
        colors: &dyn Colors,
    ) -> Option<UsernameListAction> {
        let mut action = None;
        let resp = InputField::single_line(
            "username_box_input",
            &mut self.draft,
            strings::USERNAME_INPUT_PH,
            InputFieldStyle::default_input(colors),
        )
        .max_length(MAX_USERNAME_LENGTH)
        .error(
            self.availability != UsernameAvailability::Empty
                && self.availability != UsernameAvailability::Available,
        )
        .show(ui);

        if resp.changed() {
            self.error = None;
            self.refresh_check(backend);
        }

        if let Some(ref err) = self.error {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(err)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("boxTextFgError")),
            );
        } else if self.availability != UsernameAvailability::Empty {
            ui.add_space(6.0);
            let fg = if self.availability.is_valid_public() {
                colors.color("boxTextFgGood")
            } else {
                colors.color("boxTextFgError")
            };
            ui.label(
                egui::RichText::new(self.availability.message(&self.draft))
                    .font(font_id_from_style(st::normal_font))
                    .color(fg),
            );
        }

        let clean = normalize_username(&self.draft);
        if !clean.is_empty() && self.availability == UsernameAvailability::Available {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!(
                    "{} {}",
                    tr::lng_username_link(),
                    format_username_link(clean)
                ))
                .font(FontId::new(12.0, family_regular()))
                .color(colors.color("windowActiveTextFg")),
            );
        }

        ui.add_space(10.0);
        let rules = tr::lng_username_description2().replace("**", "");
        FlatLabel::plain(&rules, FlatLabelStyle::sub_text(colors))
            .id_salt("username_rules_hint")
            .show(ui);

        // Interactive Collectible Usernames list (`edit_peer_usernames_list.cpp`)
        if !self.all_usernames.is_empty() && !self.collectibles.is_empty() {
            ui.add_space(10.0);
            divider_bar(ui, 6.0, colors);
            ui.add_space(8.0);
            FlatLabel::plain(
                strings::COLLECTIBLE_USERNAMES_HDR,
                FlatLabelStyle::subsection_title(colors),
            )
            .id_salt("collectibles_hdr")
            .show(ui);
            ui.add_space(4.0);

            let active_count = self.all_usernames.iter().filter(|u| u.is_active).count();
            let snapshot = self.all_usernames.clone();
            let mut pending_toggle: Option<(String, bool)> = None;
            let mut pending_move: Option<(usize, bool)> = None;

            for (idx, item) in snapshot.iter().enumerate() {
                let width = ui.available_width().max(240.0);
                let (row_rect, _) = ui.allocate_exact_size(Vec2::new(width, 46.0), Sense::hover());
                let badge_col = if item.is_active {
                    colors.color("windowBgActive")
                } else {
                    colors.color("windowSubTextFg")
                };
                let dot_center = Pos2::new(row_rect.left() + 16.0, row_rect.center().y);
                ui.painter().circle_filled(dot_center, 12.0, badge_col);
                ui.painter().text(
                    dot_center,
                    Align2::CENTER_CENTER,
                    "@",
                    FontId::new(12.0, family_semibold()),
                    Color32::WHITE,
                );

                let text_x = row_rect.left() + 36.0;
                ui.painter().text(
                    Pos2::new(text_x, row_rect.top() + 7.0),
                    Align2::LEFT_TOP,
                    format!("@{}", item.username),
                    font_id_from_style(st::semibold_font),
                    colors.color("windowFg"),
                );
                let status_str = if item.is_editable {
                    "Editable link"
                } else if item.is_active {
                    strings::COLLECTIBLE_ACTIVE
                } else {
                    strings::COLLECTIBLE_HIDDEN
                };
                ui.painter().text(
                    Pos2::new(text_x, row_rect.top() + 26.0),
                    Align2::LEFT_TOP,
                    status_str,
                    FontId::new(11.0, family_regular()),
                    if item.is_active {
                        colors.color("windowActiveTextFg")
                    } else {
                        colors.color("windowSubTextFg")
                    },
                );

                // Right-side controls: Copy, Toggle (for collectibles), Move Up/Down (for active items)
                let mut rx = row_rect.right() - 4.0;
                if item.is_active && active_count > 1 {
                    let down_rect = Rect::from_center_size(
                        Pos2::new(rx - 13.0, row_rect.center().y),
                        Vec2::splat(24.0),
                    );
                    let can_down = idx + 1 < active_count;
                    if small_pill_btn(
                        ui,
                        ("uname_down", &item.username),
                        down_rect,
                        "↓",
                        format!("Move @{} down", item.username),
                        can_down,
                        colors,
                    ) {
                        pending_move = Some((idx, false));
                    }
                    rx = down_rect.left() - 4.0;

                    let up_rect = Rect::from_center_size(
                        Pos2::new(rx - 13.0, row_rect.center().y),
                        Vec2::splat(24.0),
                    );
                    let can_up = idx > 0;
                    if small_pill_btn(
                        ui,
                        ("uname_up", &item.username),
                        up_rect,
                        "↑",
                        format!("Move @{} up", item.username),
                        can_up,
                        colors,
                    ) {
                        pending_move = Some((idx, true));
                    }
                    rx = up_rect.left() - 6.0;
                }

                if !item.is_editable {
                    let tog_lbl = if item.is_active { "Hide" } else { "Show" };
                    let tog_rect = Rect::from_center_size(
                        Pos2::new(rx - 24.0, row_rect.center().y),
                        Vec2::new(46.0, 24.0),
                    );
                    if small_pill_btn(
                        ui,
                        ("uname_tog", &item.username),
                        tog_rect,
                        tog_lbl,
                        format!("{tog_lbl} @{}", item.username),
                        true,
                        colors,
                    ) {
                        pending_toggle = Some((item.username.clone(), !item.is_active));
                    }
                    rx = tog_rect.left() - 6.0;
                }

                let copy_rect = Rect::from_center_size(
                    Pos2::new(rx - 24.0, row_rect.center().y),
                    Vec2::new(44.0, 24.0),
                );
                if small_pill_btn(
                    ui,
                    ("uname_copy", &item.username),
                    copy_rect,
                    "Copy",
                    format!("Copy link for @{}", item.username),
                    true,
                    colors,
                ) {
                    action = Some(UsernameListAction::CopiedLink(format_username_link(
                        &item.username,
                    )));
                }
            }

            if let Some((uname, new_active)) = pending_toggle
                && self.toggle_collectible(&uname, new_active, backend).is_ok()
            {
                action = Some(UsernameListAction::Toggled {
                    username: uname,
                    active: new_active,
                });
            } else if let Some((idx, up)) = pending_move
                && let Ok(order) = self.move_active_username(idx, up, backend)
            {
                action = Some(UsernameListAction::Reordered(order));
            }
        }

        action
    }
}

fn small_pill_btn(
    ui: &mut Ui,
    id_salt: (&str, &str),
    rect: Rect,
    label: &str,
    a11y_label: String,
    enabled: bool,
    colors: &dyn Colors,
) -> bool {
    let resp = ui.interact(
        rect,
        ui.make_persistent_id(id_salt),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, a11y_label.clone()));
    if enabled && resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let bg = if enabled && resp.hovered() {
        colors.color("windowBgActive")
    } else {
        colors.color("windowBgOver")
    };
    let fg = if !enabled {
        colors.color("windowSubTextFg")
    } else if resp.hovered() {
        Color32::WHITE
    } else {
        colors.color("windowActiveTextFg")
    };
    ui.painter().rect_filled(rect, CornerRadius::same(6), bg);
    match label {
        "↑" => tg_app_core::ui_icons::paint_arrow(
            ui.painter(),
            rect.center(),
            12.0,
            tg_app_core::ui_icons::ArrowDir::Up,
            fg,
        ),
        "↓" => tg_app_core::ui_icons::paint_arrow(
            ui.painter(),
            rect.center(),
            12.0,
            tg_app_core::ui_icons::ArrowDir::Down,
            fg,
        ),
        _ => {
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                label,
                FontId::new(11.0, family_semibold()),
                fg,
            );
        }
    }
    enabled && resp.clicked()
}
