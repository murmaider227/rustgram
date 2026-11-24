//! Invite Links manager, link editor, and link stats / importers dialog.
//!
//! Port origin:
//! - `Telegram/SourceFiles/boxes/peers/edit_peer_invite_links.cpp`
//! - `Telegram/SourceFiles/boxes/peers/edit_peer_invite_link.cpp`
//! - `Telegram/SourceFiles/ui/boxes/edit_invite_link.cpp`

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Role, Sense, Stroke, Ui, Vec2,
    WidgetInfo,
};
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

use crate::info::backend::{InfoBackend, InviteImporterInfo, InviteLinkInfo, InviteLinkRequest};
use tg_app_core::stand_in::types::PeerId;

/// Maximum invite link label length (`kMaxLabelLength = 32` in `ui/boxes/edit_invite_link.cpp`).
pub const MAX_INVITE_LABEL_LEN: usize = 32;

/// Seconds in one hour / day / week for expiry presets (`ui/boxes/edit_invite_link.cpp`).
pub const SECS_HOUR: i32 = 3_600;
/// Seconds in one day.
pub const SECS_DAY: i32 = 86_400;
/// Seconds in one week.
pub const SECS_WEEK: i32 = 86_400 * 7;

/// Visual state category for an invite link (`Color` enum in `edit_peer_invite_links.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InviteLinkVisualState {
    /// Permanent or unlimited active link.
    Permanent,
    /// Active link with an expiry or usage limit (< 75% consumed).
    Expiring,
    /// Active link nearing expiry or usage limit (>= 75% consumed).
    ExpireSoon,
    /// Link whose expiry passed or usage limit was reached.
    Expired,
    /// Revoked link.
    Revoked,
}

/// Returns `true` if `link` is expired by date or usage limit at Unix time `now`.
#[must_use]
pub const fn is_expired_link(link: &InviteLinkInfo, now: i32) -> bool {
    if link.revoked {
        return false;
    }
    if let Some(exp) = link.expire_date
        && exp > 0
        && exp <= now
    {
        return true;
    }
    if let Some(limit) = link.usage_limit
        && limit > 0
        && link.usage >= limit
    {
        return true;
    }
    false
}

/// Computes consumption progress in `0.0..=1.0` (or `-1.0` for unlimited links),
/// porting `ComputeProgress` from `edit_peer_invite_links.cpp:125-144`.
#[must_use]
pub fn compute_link_progress(link: &InviteLinkInfo, now: i32) -> f64 {
    let start_date = link.start_date.filter(|&d| d > 0).unwrap_or(link.date);
    let expire_date = link.expire_date.unwrap_or(0);
    let usage_limit = link.usage_limit.unwrap_or(0);
    if expire_date <= start_date && usage_limit <= 0 {
        return -1.0;
    }
    let expire_progress = if expire_date <= start_date || now <= start_date {
        0.0
    } else if expire_date <= now {
        1.0
    } else {
        f64::from(now - start_date) / f64::from(expire_date - start_date)
    };
    let usage_progress = if usage_limit <= 0 || link.usage <= 0 {
        0.0
    } else if usage_limit <= link.usage {
        1.0
    } else {
        f64::from(link.usage) / f64::from(usage_limit)
    };
    expire_progress.max(usage_progress)
}

/// Computes the visual state category for `link`, porting `ComputeColor` from
/// `edit_peer_invite_links.cpp:146-160`.
#[must_use]
pub fn compute_link_visual_state(link: &InviteLinkInfo, now: i32) -> InviteLinkVisualState {
    if link.revoked {
        return InviteLinkVisualState::Revoked;
    }
    let progress = compute_link_progress(link, now);
    if progress >= 1.0 {
        InviteLinkVisualState::Expired
    } else if progress >= 0.75 {
        InviteLinkVisualState::ExpireSoon
    } else if progress >= 0.0 {
        InviteLinkVisualState::Expiring
    } else {
        InviteLinkVisualState::Permanent
    }
}

/// Formats the human-readable status subtitle for an invite link, porting `ComputeStatus`
/// from `edit_peer_invite_links.cpp:162-214`.
#[must_use]
pub fn format_invite_link_status(link: &InviteLinkInfo, now: i32) -> String {
    let expired = is_expired_link(link, now);
    let mut parts = Vec::new();
    let usage_limit = link.usage_limit.unwrap_or(0);

    let base = if link.usage > 0 {
        let mut s = format!("{} joined", link.usage);
        if !expired && !link.revoked && usage_limit > link.usage {
            s.push_str(&format!(", {} remaining", usage_limit - link.usage));
        } else if !expired && !link.revoked && link.requested > 0 {
            s.push_str(&format!(", {} requested", link.requested));
        }
        s
    } else if !expired && !link.revoked && link.requested > 0 {
        format!("{} requested to join", link.requested)
    } else if !expired && !link.revoked && usage_limit > 0 {
        format!("{usage_limit} can join")
    } else {
        "No one joined yet".to_owned()
    };
    parts.push(base);

    if link.revoked {
        parts.push("Revoked".to_owned());
        return parts.join(" · ");
    }
    if expired {
        parts.push("Expired".to_owned());
        return parts.join(" · ");
    }
    if let Some(exp) = link.expire_date
        && exp > now
    {
        let left = exp - now;
        if left >= SECS_DAY {
            parts.push(format!("{}d left", left / SECS_DAY));
        } else if left >= SECS_HOUR {
            parts.push(format!("{}h left", left / SECS_HOUR));
        } else {
            parts.push(format!("{}m left", (left / 60).max(1)));
        }
    }
    if link.request_needed {
        parts.push("Approval required".to_owned());
    }
    parts.join(" · ")
}

/// Expiry selection preset matching `ui/boxes/edit_invite_link.cpp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpirePreset {
    /// Never expires.
    NoLimit,
    /// Expires 1 hour from now.
    OneHour,
    /// Expires 1 day from now.
    OneDay,
    /// Expires 1 week from now.
    OneWeek,
    /// Custom duration in hours.
    CustomHours,
}

/// Usage limit selection preset matching `ui/boxes/edit_invite_link.cpp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageLimitPreset {
    /// Unlimited joins.
    NoLimit,
    /// 1 user.
    One,
    /// 10 users.
    Ten,
    /// 100 users.
    Hundred,
    /// Custom limit.
    Custom,
}

/// Sub-form state for creating or editing a single invite link (`EditInviteLinkBox`).
#[derive(Debug, Clone)]
pub struct InviteLinkEditorState {
    /// Existing URL when editing (`None` when creating a new link).
    pub editing_link: Option<String>,
    /// Optional admin label (max 32 chars).
    pub title: String,
    /// Require admin approval (disables usage limit).
    pub request_needed: bool,
    /// Expiry duration preset.
    pub expire_preset: ExpirePreset,
    /// Custom expiry duration in hours (used when `expire_preset == CustomHours`).
    pub custom_expire_hours: String,
    /// Usage limit preset.
    pub usage_preset: UsageLimitPreset,
    /// Custom usage limit string (used when `usage_preset == Custom`).
    pub custom_usage_limit: String,
}

impl Default for InviteLinkEditorState {
    fn default() -> Self {
        Self {
            editing_link: None,
            title: String::new(),
            request_needed: false,
            expire_preset: ExpirePreset::NoLimit,
            custom_expire_hours: "24".to_owned(),
            usage_preset: UsageLimitPreset::NoLimit,
            custom_usage_limit: "50".to_owned(),
        }
    }
}

impl InviteLinkEditorState {
    /// Initializes an editor populated from an existing [`InviteLinkInfo`].
    #[must_use]
    pub fn from_existing(link: &InviteLinkInfo, now: i32) -> Self {
        let (expire_preset, custom_expire_hours) = match link.expire_date {
            Some(exp) if exp > now => {
                let hrs = ((exp - now) / SECS_HOUR).max(1);
                (ExpirePreset::CustomHours, hrs.to_string())
            }
            _ => (ExpirePreset::NoLimit, "24".to_owned()),
        };
        let (usage_preset, custom_usage_limit) = match link.usage_limit {
            None | Some(0) => (UsageLimitPreset::NoLimit, "50".to_owned()),
            Some(1) => (UsageLimitPreset::One, "1".to_owned()),
            Some(10) => (UsageLimitPreset::Ten, "10".to_owned()),
            Some(100) => (UsageLimitPreset::Hundred, "100".to_owned()),
            Some(n) => (UsageLimitPreset::Custom, n.to_string()),
        };
        Self {
            editing_link: Some(link.link.clone()),
            title: link.title.clone(),
            request_needed: link.request_needed,
            expire_preset,
            custom_expire_hours,
            usage_preset,
            custom_usage_limit,
        }
    }

    /// Converts form state into an [`InviteLinkRequest`] anchored on Unix time `now`.
    pub fn to_request(&self, now: i32) -> Result<InviteLinkRequest, String> {
        let expire_date = match self.expire_preset {
            ExpirePreset::NoLimit => None,
            ExpirePreset::OneHour => Some(now + SECS_HOUR),
            ExpirePreset::OneDay => Some(now + SECS_DAY),
            ExpirePreset::OneWeek => Some(now + SECS_WEEK),
            ExpirePreset::CustomHours => {
                let hrs = self
                    .custom_expire_hours
                    .trim()
                    .parse::<i32>()
                    .map_err(|_| "Enter a valid positive number of hours.".to_owned())?;
                if hrs <= 0 {
                    return Err("Expiry duration must be at least 1 hour.".to_owned());
                }
                Some(now.saturating_add(hrs.saturating_mul(SECS_HOUR)))
            }
        };
        let usage_limit = if self.request_needed {
            None
        } else {
            match self.usage_preset {
                UsageLimitPreset::NoLimit => None,
                UsageLimitPreset::One => Some(1),
                UsageLimitPreset::Ten => Some(10),
                UsageLimitPreset::Hundred => Some(100),
                UsageLimitPreset::Custom => {
                    let n = self
                        .custom_usage_limit
                        .trim()
                        .parse::<i32>()
                        .map_err(|_| "Enter a valid positive user limit.".to_owned())?;
                    if n <= 0 {
                        return Err("User limit must be at least 1.".to_owned());
                    }
                    Some(n)
                }
            }
        };
        let title: String = self
            .title
            .trim()
            .chars()
            .take(MAX_INVITE_LABEL_LEN)
            .collect();
        Ok(InviteLinkRequest {
            title,
            expire_date,
            usage_limit,
            request_needed: self.request_needed,
            legacy_revoke_permanent: false,
        })
    }
}

/// Active view inside [`EditInviteLinksBoxState`].
#[derive(Debug, Clone)]
pub enum InviteLinksView {
    /// Main list of primary, additional active, and revoked links.
    List,
    /// Create or edit link form.
    Editor(InviteLinkEditorState),
    /// Per-link stats, joined importers, and pending join requests.
    Stats {
        /// Selected link metadata.
        link: InviteLinkInfo,
        /// Users who joined via this link.
        joined: Vec<InviteImporterInfo>,
        /// Users awaiting approval via this link.
        requested: Vec<InviteImporterInfo>,
    },
}

/// Events emitted by [`EditInviteLinksBoxState::show_body`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InviteLinksBoxAction {
    /// Toast message for the host (e.g. copied link, revoked, deleted).
    Toast(String),
    /// Refresh modal buttons because view switched between List / Editor / Stats.
    RefreshModal,
    /// Invite links changed on the backend.
    LinksChanged,
}

/// Modal box state for managing invite links (`EditInviteLinksBox`).
#[derive(Debug, Clone)]
pub struct EditInviteLinksBoxState {
    /// Target group or channel peer identifier.
    pub peer_id: PeerId,
    /// Display title of the peer.
    pub peer_title: String,
    /// Active invite links (primary permanent link first when present).
    pub active_links: Vec<InviteLinkInfo>,
    /// Revoked invite links.
    pub revoked_links: Vec<InviteLinkInfo>,
    /// Whether initial data has been fetched from the backend.
    pub loaded: bool,
    /// Current sub-view.
    pub view: InviteLinksView,
    /// Last error message to display inline.
    pub error: Option<String>,
    /// Reference Unix timestamp used for expiry/progress calculations.
    pub now_unix: i32,
}

impl EditInviteLinksBoxState {
    /// Creates a new invite links manager box for `peer_id`.
    #[must_use]
    pub fn new(peer_id: PeerId, peer_title: impl Into<String>) -> Self {
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1_795_348_800, |d| d.as_secs() as i32);
        Self {
            peer_id,
            peer_title: peer_title.into(),
            active_links: Vec::new(),
            revoked_links: Vec::new(),
            loaded: false,
            view: InviteLinksView::List,
            error: None,
            now_unix,
        }
    }

    /// Loads active and revoked invite links from `backend`.
    pub fn reload(&mut self, backend: &dyn InfoBackend) {
        match backend.exported_chat_invites(self.peer_id, false) {
            Ok(active) => {
                self.active_links = active.links;
                self.error = None;
            }
            Err(err) => {
                self.error = Some(err);
            }
        }
        if let Ok(revoked) = backend.exported_chat_invites(self.peer_id, true) {
            self.revoked_links = revoked.links;
        }
        self.loaded = true;
    }

    /// Opens the "Create a New Link" sub-form.
    pub fn open_create_form(&mut self) {
        self.error = None;
        self.view = InviteLinksView::Editor(InviteLinkEditorState::default());
    }

    /// Opens the edit sub-form for an existing invite link.
    pub fn open_edit_form(&mut self, link: &InviteLinkInfo) {
        self.error = None;
        self.view =
            InviteLinksView::Editor(InviteLinkEditorState::from_existing(link, self.now_unix));
    }

    /// Opens the stats & importers view for `link`, fetching joined + requested importers.
    pub fn open_stats(&mut self, link: InviteLinkInfo, backend: &dyn InfoBackend) {
        let joined = backend
            .chat_invite_importers(self.peer_id, Some(&link.link), false, "")
            .map(|r| r.importers)
            .unwrap_or_default();
        let requested = backend
            .chat_invite_importers(self.peer_id, Some(&link.link), true, "")
            .map(|r| r.importers)
            .unwrap_or_default();
        self.error = None;
        self.view = InviteLinksView::Stats {
            link,
            joined,
            requested,
        };
    }

    /// Saves the currently open create/edit invite link form via `backend`.
    pub fn submit_editor(
        &mut self,
        backend: &mut dyn InfoBackend,
    ) -> Result<InviteLinkInfo, String> {
        let InviteLinksView::Editor(ref form) = self.view else {
            return Err("Link editor is not active.".to_owned());
        };
        let req = match form.to_request(self.now_unix) {
            Ok(r) => r,
            Err(err) => {
                self.error = Some(err.clone());
                return Err(err);
            }
        };
        let editing_url = form.editing_link.clone();
        let res = match editing_url {
            Some(url) => backend.edit_exported_chat_invite(self.peer_id, &url, req),
            None => backend.export_chat_invite(self.peer_id, req),
        };
        match res {
            Ok(updated) => {
                self.error = None;
                self.view = InviteLinksView::List;
                self.reload(backend);
                Ok(updated)
            }
            Err(err) => {
                self.error = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Revokes `link` via `backend` and refreshes the list.
    pub fn revoke_link(&mut self, link: &str, backend: &mut dyn InfoBackend) -> Result<(), String> {
        match backend.revoke_exported_chat_invite(self.peer_id, link) {
            Ok(_) => {
                self.error = None;
                self.view = InviteLinksView::List;
                self.reload(backend);
                Ok(())
            }
            Err(err) => {
                self.error = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Permanently deletes a revoked `link` via `backend`.
    pub fn delete_link(&mut self, link: &str, backend: &mut dyn InfoBackend) -> Result<(), String> {
        match backend.delete_exported_chat_invite(self.peer_id, link) {
            Ok(()) => {
                self.error = None;
                self.reload(backend);
                Ok(())
            }
            Err(err) => {
                self.error = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Permanently deletes all revoked links via `backend`.
    pub fn delete_all_revoked(&mut self, backend: &mut dyn InfoBackend) -> Result<(), String> {
        match backend.delete_revoked_exported_chat_invites(self.peer_id) {
            Ok(()) => {
                self.error = None;
                self.reload(backend);
                Ok(())
            }
            Err(err) => {
                self.error = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor matching the active sub-view.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        match &self.view {
            InviteLinksView::List => BoxModal::new(id, tr::lng_manage_peer_invite_links())
                .width(st::box_wide_width.0 as f32 + 32.0)
                .add_button(BoxButton::default_btn("close", tr::lng_close())),
            InviteLinksView::Editor(form) => {
                let title = if form.editing_link.is_some() {
                    tr::lng_group_invite_edit_title()
                } else {
                    tr::lng_group_invite_new_title()
                };
                let save_lbl = if form.editing_link.is_some() {
                    tr::lng_settings_save()
                } else {
                    "Create".to_owned()
                };
                BoxModal::new(id, title)
                    .width(st::box_wide_width.0 as f32 + 32.0)
                    .with_left_button(BoxButton::default_btn("back_list", "Back"))
                    .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
                    .add_button(BoxButton::default_btn("save_link", save_lbl))
            }
            InviteLinksView::Stats { link, .. } => {
                let heading = if link.title.is_empty() {
                    "Invite Link".to_owned()
                } else {
                    link.title.clone()
                };
                let mut modal = BoxModal::new(id, heading)
                    .width(st::box_wide_width.0 as f32 + 32.0)
                    .with_left_button(BoxButton::default_btn("back_list", "Back"))
                    .add_button(BoxButton::default_btn("close", tr::lng_close()));
                if !link.revoked && !link.permanent {
                    modal = modal.add_button(BoxButton::default_btn("edit_stats_link", "Edit"));
                }
                modal
            }
        }
    }

    /// Renders the Invite Links modal body and processes inline row actions.
    pub fn show_body(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn InfoBackend,
        colors: &dyn Colors,
    ) -> Option<InviteLinksBoxAction> {
        if !self.loaded {
            self.reload(backend);
        }

        let action = match self.view.clone() {
            InviteLinksView::List => self.show_list_body(ui, backend, colors),
            InviteLinksView::Editor(_) => self.show_editor_body(ui, colors),
            InviteLinksView::Stats {
                link,
                joined,
                requested,
            } => self.show_stats_body(ui, &link, &joined, &requested, backend, colors),
        };

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

    fn show_list_body(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn InfoBackend,
        colors: &dyn Colors,
    ) -> Option<InviteLinksBoxAction> {
        let mut action = None;

        // 1. Primary permanent invite link card
        let primary = self
            .active_links
            .iter()
            .find(|l| l.permanent && !l.revoked)
            .cloned();

        FlatLabel::plain("Invite Link", FlatLabelStyle::subsection_title(colors))
            .id_salt("inv_primary_hdr")
            .show(ui);
        ui.add_space(6.0);

        if let Some(prim) = primary {
            let box_w = ui.available_width().max(220.0);
            let (card_rect, _) = ui.allocate_exact_size(Vec2::new(box_w, 42.0), Sense::hover());
            ui.painter().rect_filled(
                card_rect,
                CornerRadius::same(8),
                colors.color("windowBgOver"),
            );
            ui.painter().text(
                Pos2::new(card_rect.min.x + 12.0, card_rect.center().y),
                Align2::LEFT_CENTER,
                &prim.link,
                font_id_from_style(st::semibold_font),
                colors.color("windowBoldFg"),
            );

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if inline_action_pill(
                    ui,
                    "inv_copy_primary",
                    "Copy Link",
                    colors.color("activeButtonBg"),
                    Color32::WHITE,
                ) {
                    action = Some(InviteLinksBoxAction::Toast(format!(
                        "Invite link copied: {}",
                        prim.link
                    )));
                }
                ui.add_space(6.0);
                if inline_action_pill(
                    ui,
                    "inv_stats_primary",
                    &format!("Stats ({})", prim.usage),
                    colors.color("windowBgOver"),
                    colors.color("windowActiveTextFg"),
                ) {
                    self.open_stats(prim.clone(), backend);
                    action = Some(InviteLinksBoxAction::RefreshModal);
                }
                ui.add_space(6.0);
                if inline_action_pill(
                    ui,
                    "inv_revoke_primary",
                    "Revoke",
                    colors.color("windowBgOver"),
                    colors.color("attentionButtonFg"),
                ) && self.revoke_link(&prim.link, backend).is_ok()
                {
                    action = Some(InviteLinksBoxAction::LinksChanged);
                }
            });
        } else {
            ui.label(
                egui::RichText::new("No primary permanent link yet.")
                    .font(FontId::new(12.5, family_regular()))
                    .color(colors.color("windowSubTextFg")),
            );
        }

        ui.add_space(10.0);
        divider_bar(ui, 6.0, colors);
        ui.add_space(8.0);

        // 2. Additional active links + "Create a New Link" button
        ui.horizontal(|ui| {
            FlatLabel::plain("Additional Links", FlatLabelStyle::subsection_title(colors))
                .id_salt("inv_extra_hdr")
                .show(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if inline_action_pill(
                    ui,
                    "inv_create_new",
                    "+ Create a New Link",
                    colors.color("activeButtonBg"),
                    Color32::WHITE,
                ) {
                    self.open_create_form();
                    action = Some(InviteLinksBoxAction::RefreshModal);
                }
            });
        });
        ui.add_space(6.0);

        let extras: Vec<InviteLinkInfo> = self
            .active_links
            .iter()
            .filter(|l| !l.permanent)
            .cloned()
            .collect();
        if extras.is_empty() {
            ui.label(
                egui::RichText::new(
                    "Create additional links with expiry times, usage limits, or admin approval.",
                )
                .font(FontId::new(12.5, family_regular()))
                .color(colors.color("windowSubTextFg")),
            );
        } else {
            for link in &extras {
                if let Some(ev) = self.render_invite_row(ui, link, false, backend, colors) {
                    action = Some(ev);
                }
            }
        }

        // 3. Revoked links section
        if !self.revoked_links.is_empty() {
            ui.add_space(10.0);
            divider_bar(ui, 6.0, colors);
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                FlatLabel::plain(
                    &format!("Revoked Links ({})", self.revoked_links.len()),
                    FlatLabelStyle::subsection_title(colors),
                )
                .id_salt("inv_revoked_hdr")
                .show(ui);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if inline_action_pill(
                        ui,
                        "inv_delete_all_revoked",
                        "Delete All Revoked",
                        colors.color("windowBgOver"),
                        colors.color("attentionButtonFg"),
                    ) && self.delete_all_revoked(backend).is_ok()
                    {
                        action = Some(InviteLinksBoxAction::LinksChanged);
                    }
                });
            });
            ui.add_space(6.0);

            let revoked_snapshot = self.revoked_links.clone();
            for link in &revoked_snapshot {
                if let Some(ev) = self.render_invite_row(ui, link, true, backend, colors) {
                    action = Some(ev);
                }
            }
        }

        action
    }

    fn render_invite_row(
        &mut self,
        ui: &mut Ui,
        link: &InviteLinkInfo,
        is_revoked: bool,
        backend: &mut dyn InfoBackend,
        colors: &dyn Colors,
    ) -> Option<InviteLinksBoxAction> {
        let mut action = None;
        let width = ui.available_width().max(220.0);
        let row_h = 56.0;
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, row_h), Sense::hover());
        if resp.hovered() {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(6), colors.color("windowBgOver"));
        }

        let vis = compute_link_visual_state(link, self.now_unix);
        let badge_col = match vis {
            InviteLinkVisualState::Permanent => colors.color("activeButtonBg"),
            InviteLinkVisualState::Expiring => Color32::from_rgb(0x4E, 0xAE, 0x51),
            InviteLinkVisualState::ExpireSoon => Color32::from_rgb(0xE0, 0x91, 0x2F),
            InviteLinkVisualState::Expired => colors.color("attentionButtonFg"),
            InviteLinkVisualState::Revoked => colors.color("windowSubTextFg"),
        };

        let dot_center = Pos2::new(rect.min.x + 20.0, rect.center().y);
        ui.painter().circle_filled(dot_center, 15.0, badge_col);
        if link.request_needed {
            tg_app_core::ui_icons::paint_check(ui.painter(), dot_center, 13.0, Color32::WHITE);
        } else {
            tg_app_core::ui_icons::paint_style_tinted(
                ui.painter(),
                Rect::from_center_size(dot_center, Vec2::splat(16.0)),
                st::menu_icon_link,
                Color32::WHITE,
            );
        }

        let right_x = rect.max.x - 4.0;
        let btn1_rect = Rect::from_center_size(
            Pos2::new(right_x - 28.0, rect.center().y),
            Vec2::new(52.0, 26.0),
        );
        let btn1_lbl = if is_revoked { "Delete" } else { "Revoke" };
        let btn1_resp = ui.interact(
            btn1_rect,
            ui.make_persistent_id(("inv_row_act1", &link.link)),
            Sense::click(),
        );
        btn1_resp.widget_info(|| {
            WidgetInfo::labeled(Role::Button, true, format!("{btn1_lbl} {}", link.link))
        });
        if btn1_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter().rect_filled(
                btn1_rect,
                CornerRadius::same(6),
                colors.color("windowBgRipple"),
            );
        }
        ui.painter().text(
            btn1_rect.center(),
            Align2::CENTER_CENTER,
            btn1_lbl,
            FontId::new(11.5, family_semibold()),
            colors.color("attentionButtonFg"),
        );
        if btn1_resp.clicked() {
            let ok = if is_revoked {
                self.delete_link(&link.link, backend).is_ok()
            } else {
                self.revoke_link(&link.link, backend).is_ok()
            };
            if ok {
                action = Some(InviteLinksBoxAction::LinksChanged);
            }
        }

        let mut text_right = btn1_rect.min.x - 6.0;
        if !is_revoked {
            let edit_rect = Rect::from_center_size(
                Pos2::new(text_right - 22.0, rect.center().y),
                Vec2::new(42.0, 26.0),
            );
            let edit_resp = ui.interact(
                edit_rect,
                ui.make_persistent_id(("inv_row_edit", &link.link)),
                Sense::click(),
            );
            edit_resp.widget_info(|| {
                WidgetInfo::labeled(Role::Button, true, format!("Edit {}", link.link))
            });
            if edit_resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                ui.painter().rect_filled(
                    edit_rect,
                    CornerRadius::same(6),
                    colors.color("windowBgRipple"),
                );
            }
            ui.painter().text(
                edit_rect.center(),
                Align2::CENTER_CENTER,
                "Edit",
                FontId::new(11.5, family_semibold()),
                colors.color("windowActiveTextFg"),
            );
            if edit_resp.clicked() {
                self.open_edit_form(link);
                action = Some(InviteLinksBoxAction::RefreshModal);
            }
            text_right = edit_rect.min.x - 6.0;
        }

        let text_x = dot_center.x + 24.0;
        let info_rect = Rect::from_min_max(
            Pos2::new(text_x, rect.min.y + 4.0),
            Pos2::new(text_right.max(text_x + 40.0), rect.max.y - 4.0),
        );
        let open_resp = ui.interact(
            info_rect,
            ui.make_persistent_id(("inv_row_open", &link.link)),
            Sense::click(),
        );
        let display_title = if link.title.is_empty() {
            link.link
                .trim_start_matches("https://")
                .trim_start_matches("t.me/+")
                .to_owned()
        } else {
            link.title.clone()
        };
        open_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, &display_title));
        if open_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if open_resp.clicked() && action.is_none() {
            self.open_stats(link.clone(), backend);
            action = Some(InviteLinksBoxAction::RefreshModal);
        }

        let clip = ui.painter().with_clip_rect(info_rect);
        clip.text(
            Pos2::new(text_x, rect.min.y + 8.0),
            Align2::LEFT_TOP,
            &display_title,
            font_id_from_style(st::semibold_font),
            colors.color("windowBoldFg"),
        );
        let status = format_invite_link_status(link, self.now_unix);
        clip.text(
            Pos2::new(text_x, rect.min.y + 28.0),
            Align2::LEFT_TOP,
            status,
            FontId::new(12.0, family_regular()),
            colors.color("windowSubTextFg"),
        );

        ui.painter().line_segment(
            [
                Pos2::new(text_x, rect.max.y),
                Pos2::new(rect.max.x - 4.0, rect.max.y),
            ],
            Stroke::new(0.5, colors.color("shadowFg")),
        );

        action
    }

    fn show_editor_body(
        &mut self,
        ui: &mut Ui,
        colors: &dyn Colors,
    ) -> Option<InviteLinksBoxAction> {
        let InviteLinksView::Editor(ref mut form) = self.view else {
            return None;
        };
        let input_st = InputFieldStyle::default_input(colors);
        let cb_st = CheckboxStyle::box_checkbox(colors);
        let rad_st = RadioStyle::default_radio(colors);

        // 1. Optional link label
        InputField::single_line(
            "inv_editor_label",
            &mut form.title,
            "Link Name (optional)",
            input_st,
        )
        .max_length(MAX_INVITE_LABEL_LEN)
        .show(ui);
        ui.label(
            egui::RichText::new("Only admins will see this link label.")
                .font(FontId::new(11.5, family_regular()))
                .color(colors.color("windowSubTextFg")),
        );

        ui.add_space(10.0);
        // 2. Request Admin Approval toggle (mutually exclusive with usage limit)
        Toggle::new(
            &mut form.request_needed,
            ToggleStyle::default_toggle(colors),
        )
        .with_label("Request Admin Approval", colors.color("windowFg"))
        .show(ui);
        ui.label(
            egui::RichText::new(if form.request_needed {
                "Admins must approve users who join through this link."
            } else {
                "Anyone with this link can join immediately."
            })
            .font(FontId::new(11.5, family_regular()))
            .color(colors.color("windowSubTextFg")),
        );

        ui.add_space(10.0);
        divider_bar(ui, 6.0, colors);
        ui.add_space(8.0);

        // 3. Limit by time period
        FlatLabel::plain(
            "Limit by Time Period",
            FlatLabelStyle::subsection_title(colors),
        )
        .id_salt("inv_exp_hdr")
        .show(ui);
        ui.add_space(4.0);
        for (preset, lbl) in [
            (ExpirePreset::NoLimit, "No limit"),
            (ExpirePreset::OneHour, "1 hour"),
            (ExpirePreset::OneDay, "1 day"),
            (ExpirePreset::OneWeek, "1 week"),
            (ExpirePreset::CustomHours, "Custom hours"),
        ] {
            Radio::new(&mut form.expire_preset, preset, lbl, cb_st, rad_st).show(ui);
        }
        if form.expire_preset == ExpirePreset::CustomHours {
            InputField::single_line(
                "inv_custom_hours",
                &mut form.custom_expire_hours,
                "Duration in hours",
                input_st,
            )
            .show(ui);
        }

        // 4. Limit by number of users (hidden when request_needed == true)
        if !form.request_needed {
            ui.add_space(10.0);
            divider_bar(ui, 6.0, colors);
            ui.add_space(8.0);

            FlatLabel::plain(
                "Limit by Number of Users",
                FlatLabelStyle::subsection_title(colors),
            )
            .id_salt("inv_usage_hdr")
            .show(ui);
            ui.add_space(4.0);
            for (preset, lbl) in [
                (UsageLimitPreset::NoLimit, "No limit"),
                (UsageLimitPreset::One, "1 user"),
                (UsageLimitPreset::Ten, "10 users"),
                (UsageLimitPreset::Hundred, "100 users"),
                (UsageLimitPreset::Custom, "Custom limit"),
            ] {
                Radio::new(&mut form.usage_preset, preset, lbl, cb_st, rad_st).show(ui);
            }
            if form.usage_preset == UsageLimitPreset::Custom {
                InputField::single_line(
                    "inv_custom_limit",
                    &mut form.custom_usage_limit,
                    "Maximum users",
                    input_st,
                )
                .show(ui);
            }
        }

        None
    }

    fn show_stats_body(
        &mut self,
        ui: &mut Ui,
        link: &InviteLinkInfo,
        joined: &[InviteImporterInfo],
        requested: &[InviteImporterInfo],
        backend: &mut dyn InfoBackend,
        colors: &dyn Colors,
    ) -> Option<InviteLinksBoxAction> {
        let mut action = None;

        let box_w = ui.available_width().max(220.0);
        let (card_rect, _) = ui.allocate_exact_size(Vec2::new(box_w, 40.0), Sense::hover());
        ui.painter().rect_filled(
            card_rect,
            CornerRadius::same(8),
            colors.color("windowBgOver"),
        );
        ui.painter().text(
            Pos2::new(card_rect.min.x + 12.0, card_rect.center().y),
            Align2::LEFT_CENTER,
            &link.link,
            font_id_from_style(st::semibold_font),
            colors.color("windowBoldFg"),
        );

        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(format!(
                "Created by {} · {}",
                link.admin_name,
                format_invite_link_status(link, self.now_unix)
            ))
            .font(FontId::new(12.0, family_regular()))
            .color(colors.color("windowSubTextFg")),
        );

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if inline_action_pill(
                ui,
                "inv_stats_copy",
                "Copy Link",
                colors.color("activeButtonBg"),
                Color32::WHITE,
            ) {
                action = Some(InviteLinksBoxAction::Toast(format!(
                    "Invite link copied: {}",
                    link.link
                )));
            }
            if !link.revoked {
                ui.add_space(6.0);
                if inline_action_pill(
                    ui,
                    "inv_stats_revoke",
                    "Revoke Link",
                    colors.color("windowBgOver"),
                    colors.color("attentionButtonFg"),
                ) && self.revoke_link(&link.link, backend).is_ok()
                {
                    action = Some(InviteLinksBoxAction::LinksChanged);
                }
            }
        });

        if !requested.is_empty() {
            ui.add_space(10.0);
            divider_bar(ui, 6.0, colors);
            ui.add_space(6.0);
            FlatLabel::plain(
                &format!("Join Requests ({})", requested.len()),
                FlatLabelStyle::subsection_title(colors),
            )
            .id_salt("inv_stats_req_hdr")
            .show(ui);
            ui.add_space(4.0);
            for req_user in requested {
                if let Some(approved) = render_importer_row(ui, req_user, true, colors)
                    && backend
                        .hide_chat_join_request(self.peer_id, req_user.user_id, approved)
                        .is_ok()
                {
                    self.open_stats(link.clone(), backend);
                    action = Some(InviteLinksBoxAction::LinksChanged);
                }
            }
        }

        ui.add_space(10.0);
        divider_bar(ui, 6.0, colors);
        ui.add_space(6.0);
        FlatLabel::plain(
            &format!("Joined ({})", joined.len()),
            FlatLabelStyle::subsection_title(colors),
        )
        .id_salt("inv_stats_joined_hdr")
        .show(ui);
        ui.add_space(4.0);
        if joined.is_empty() {
            ui.label(
                egui::RichText::new("No one has joined via this link yet.")
                    .font(FontId::new(12.5, family_regular()))
                    .color(colors.color("windowSubTextFg")),
            );
        } else {
            for user in joined {
                let _ = render_importer_row(ui, user, false, colors);
            }
        }

        action
    }
}

fn render_importer_row(
    ui: &mut Ui,
    user: &InviteImporterInfo,
    show_request_buttons: bool,
    colors: &dyn Colors,
) -> Option<bool> {
    let mut decision = None;
    let width = ui.available_width().max(200.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 46.0), Sense::hover());

    let av_rect = Rect::from_center_size(
        Pos2::new(rect.min.x + 18.0, rect.center().y),
        Vec2::splat(34.0),
    );
    EmptyUserpic::for_color_index(user.color_index, &user.name, colors)
        .paint_circle(ui.painter(), av_rect);

    let text_x = av_rect.max.x + 10.0;
    ui.painter().text(
        Pos2::new(text_x, rect.min.y + 6.0),
        Align2::LEFT_TOP,
        &user.name,
        font_id_from_style(st::semibold_font),
        colors.color("windowBoldFg"),
    );

    let sub = if user.via_chatlist {
        "Joined via folder link".to_owned()
    } else if let Some(ref uname) = user.username {
        format!("@{uname}")
    } else {
        "Joined via invite link".to_owned()
    };
    ui.painter().text(
        Pos2::new(text_x, rect.min.y + 24.0),
        Align2::LEFT_TOP,
        sub,
        FontId::new(11.5, family_regular()),
        colors.color("windowSubTextFg"),
    );

    if show_request_buttons {
        let rej_rect = Rect::from_center_size(
            Pos2::new(rect.max.x - 30.0, rect.center().y),
            Vec2::new(54.0, 24.0),
        );
        let acc_rect = Rect::from_center_size(
            Pos2::new(rej_rect.min.x - 36.0, rect.center().y),
            Vec2::new(60.0, 24.0),
        );
        let acc_resp = ui.interact(
            acc_rect,
            ui.make_persistent_id(("inv_imp_acc", user.user_id.0)),
            Sense::click(),
        );
        if acc_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        ui.painter().rect_filled(
            acc_rect,
            CornerRadius::same(6),
            colors.color("activeButtonBg"),
        );
        ui.painter().text(
            acc_rect.center(),
            Align2::CENTER_CENTER,
            "Approve",
            FontId::new(11.0, family_semibold()),
            Color32::WHITE,
        );
        if acc_resp.clicked() {
            decision = Some(true);
        }

        let rej_resp = ui.interact(
            rej_rect,
            ui.make_persistent_id(("inv_imp_rej", user.user_id.0)),
            Sense::click(),
        );
        if rej_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        ui.painter().rect_filled(
            rej_rect,
            CornerRadius::same(6),
            colors.color("windowBgOver"),
        );
        ui.painter().text(
            rej_rect.center(),
            Align2::CENTER_CENTER,
            "Dismiss",
            FontId::new(11.0, family_semibold()),
            colors.color("attentionButtonFg"),
        );
        if rej_resp.clicked() {
            decision = Some(false);
        }
    }

    decision
}

fn inline_action_pill(ui: &mut Ui, id_salt: &str, label: &str, bg: Color32, fg: Color32) -> bool {
    let font = FontId::new(12.0, family_semibold());
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, fg);
    let size = Vec2::new(galley.size().x + 18.0, 28.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let _ = id_salt;
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
