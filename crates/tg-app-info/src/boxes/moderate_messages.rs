//! Group message moderation modal (`ModerateMessagesBox`): delete selected messages,
//! report spam, delete all messages / reactions from authors, and ban or restrict participants
//! (with optional community-wide ban).
//!
//! Port origin: `Telegram/SourceFiles/boxes/moderate_messages_box.h`
//! and `Telegram/SourceFiles/boxes/moderate_messages_box.cpp`.

use std::collections::{HashMap, HashSet};

use egui::{FontId, Ui};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold, font_id_from_style};
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{CheckStyle, CheckboxStyle, Colors, FlatLabelStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::checkbox::Checkbox;
use tg_ui::widgets::labels::{FlatLabel, divider_bar};

use crate::info::backend::InfoBackend;
use tg_app_core::stand_in::types::PeerId;

pub use tg_app_core::core_types::dto::moderate_messages::*;

/// Stateful `ModerateMessagesBox` modal (`boxes/moderate_messages_box.cpp`).
#[derive(Debug, Clone)]
pub struct ModerateMessagesBoxState {
    /// Group / supergroup peer ID.
    pub peer_id: PeerId,
    /// Group display title.
    pub peer_title: String,
    /// Selected message IDs to delete.
    pub message_ids: Vec<i32>,
    /// Distinct authors of the selected messages.
    pub participants: Vec<ModerateParticipant>,
    /// Optional linked community ID (enables "Ban from community" option).
    pub linked_community_id: Option<PeerId>,
    /// Optional linked community title.
    pub linked_community_title: Option<String>,
    /// Master toggle: Report spam.
    pub report_spam: bool,
    /// Master toggle: Delete all messages from selected authors.
    pub delete_all_messages: bool,
    /// Master toggle: Delete all reactions from selected authors.
    pub delete_all_reactions: bool,
    /// Master toggle: Ban / restrict authors.
    pub ban_or_restrict: bool,
    /// Master toggle: Also ban from linked community.
    pub ban_from_community: bool,
    /// Selected ban/restrict duration preset.
    pub restrict_preset: RestrictDurationPreset,
    /// Per-action participant selection overrides (for multi-participant mode).
    pub report_checked: HashSet<PeerId>,
    /// Per-participant selection for deleting all messages.
    pub delete_all_checked: HashSet<PeerId>,
    /// Per-participant selection for deleting all reactions.
    pub delete_reactions_checked: HashSet<PeerId>,
    /// Per-participant selection for banning/restricting.
    pub ban_checked: HashSet<PeerId>,
    /// Expandable sub-list visibility states.
    pub report_expanded: bool,
    /// Expandable sub-list for delete-all options.
    pub delete_all_expanded: bool,
    /// Expandable sub-list for ban participants.
    pub ban_expanded: bool,
    /// Optional error banner.
    pub error: Option<String>,
}

impl ModerateMessagesBoxState {
    /// Constructs a [`ModerateMessagesBoxState`] for a group message selection.
    #[must_use]
    pub fn new(
        peer_id: PeerId,
        peer_title: impl Into<String>,
        message_ids: Vec<i32>,
        participants: Vec<ModerateParticipant>,
    ) -> Self {
        let all_ids: HashSet<PeerId> = participants.iter().map(|p| p.peer_id).collect();
        let ban_ids: HashSet<PeerId> = participants
            .iter()
            .filter(|p| p.can_ban)
            .map(|p| p.peer_id)
            .collect();
        let del_ids: HashSet<PeerId> = participants
            .iter()
            .filter(|p| p.can_delete_all)
            .map(|p| p.peer_id)
            .collect();
        Self {
            peer_id,
            peer_title: peer_title.into(),
            message_ids,
            participants,
            linked_community_id: None,
            linked_community_title: None,
            report_spam: false,
            delete_all_messages: false,
            delete_all_reactions: false,
            ban_or_restrict: false,
            ban_from_community: false,
            restrict_preset: RestrictDurationPreset::Forever,
            report_checked: all_ids.clone(),
            delete_all_checked: del_ids,
            delete_reactions_checked: all_ids,
            ban_checked: ban_ids,
            report_expanded: false,
            delete_all_expanded: false,
            ban_expanded: false,
            error: None,
        }
    }

    /// Attaches an optional linked community so admins can also ban from the whole community.
    #[must_use]
    pub fn with_linked_community(
        mut self,
        community_id: PeerId,
        community_title: impl Into<String>,
    ) -> Self {
        self.linked_community_id = Some(community_id);
        self.linked_community_title = Some(community_title.into());
        self
    }

    /// Returns `true` if any participant allows deleting all messages.
    #[must_use]
    pub fn can_delete_all(&self) -> bool {
        self.participants.iter().any(|p| p.can_delete_all)
    }

    /// Returns `true` if any participant can be banned or restricted.
    #[must_use]
    pub fn can_ban_any(&self) -> bool {
        self.participants.iter().any(|p| p.can_ban)
    }

    /// Computes total messages to be deleted (exact port of `makeMessageTitleData`
    /// in `moderate_messages_box.cpp:1092-1112`).
    #[must_use]
    pub fn effective_delete_count(&self) -> usize {
        let mut count = self.message_ids.len();
        if !self.delete_all_messages {
            return count;
        }
        let selected_by_peer: HashMap<PeerId, usize> = self
            .participants
            .iter()
            .map(|p| (p.peer_id, p.selected_msg_ids.len()))
            .collect();
        for p in &self.participants {
            if p.can_delete_all
                && self.delete_all_checked.contains(&p.peer_id)
                && let Some(total) = p.total_messages_count
            {
                let sel = selected_by_peer.get(&p.peer_id).copied().unwrap_or(0);
                count = count.saturating_add(total as usize).saturating_sub(sel);
            }
        }
        count
    }

    /// Computes the dynamic modal title (e.g. `"Delete 1 message?"` / `"Delete 14 messages?"`).
    #[must_use]
    pub fn modal_title(&self) -> String {
        let count = self.effective_delete_count();
        if count <= 1 {
            "Delete 1 message?".to_owned()
        } else {
            format!("Delete {count} messages?")
        }
    }

    /// Builds the [`ModerateMessagesRequest`] from current toggle & participant states.
    #[must_use]
    pub fn build_request(&self) -> ModerateMessagesRequest {
        let collect = |enabled: bool, set: &HashSet<PeerId>| -> Vec<PeerId> {
            if !enabled {
                return Vec::new();
            }
            self.participants
                .iter()
                .filter(|p| set.contains(&p.peer_id))
                .map(|p| p.peer_id)
                .collect()
        };
        let ban_peers = collect(self.ban_or_restrict, &self.ban_checked);
        let ban_in_community_id = if self.ban_or_restrict && self.ban_from_community {
            self.linked_community_id
        } else {
            None
        };
        ModerateMessagesRequest {
            peer_id: self.peer_id,
            message_ids: self.message_ids.clone(),
            report_spam_peers: collect(self.report_spam, &self.report_checked),
            delete_all_from_peers: collect(self.delete_all_messages, &self.delete_all_checked),
            delete_all_reactions_peers: collect(
                self.delete_all_reactions,
                &self.delete_reactions_checked,
            ),
            ban_peers,
            restrict_duration_secs: self.restrict_preset.duration_secs(),
            ban_in_community_id,
        }
    }

    /// Executes the moderation actions against `backend`.
    pub fn submit(
        &mut self,
        backend: &mut dyn InfoBackend,
    ) -> Result<ModerateMessagesResult, String> {
        let req = self.build_request();
        match backend.moderate_messages(req) {
            Ok(res) => {
                self.error = None;
                Ok(res)
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
        BoxModal::new(id, self.modal_title())
            .width(st::box_width.0 as f32 + 52.0)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::attention(
                "moderate_confirm",
                tr::lng_box_delete(),
            ))
    }

    /// Renders the moderation options checkboxes, expandable participant sub-lists,
    /// and ban duration selector.
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) {
        let cb_st = CheckboxStyle::box_checkbox(colors);
        let check_st = CheckStyle::default_check(colors);
        let is_single = self.participants.len() == 1;
        let single_name = self
            .participants
            .first()
            .map_or("user", |p| p.name.as_str())
            .to_owned();

        let subtitle = if is_single {
            format!(
                "Moderate message(s) from {single_name} in {}.",
                self.peer_title
            )
        } else {
            format!(
                "Moderate {} message(s) from {} participants in {}.",
                self.message_ids.len(),
                self.participants.len(),
                self.peer_title
            )
        };
        FlatLabel::plain(&subtitle, FlatLabelStyle::box_label(colors))
            .id_salt("moderate_subtitle")
            .show(ui);
        ui.add_space(10.0);

        // 1. Report spam
        ui.horizontal(|ui| {
            Checkbox::new(&mut self.report_spam, "Report spam", cb_st, check_st).show(ui);
            if !is_single
                && ui
                    .small_button(format!("{}", self.report_checked.len()))
                    .clicked()
            {
                self.report_expanded = !self.report_expanded;
            }
        });
        if !is_single && self.report_expanded {
            render_participant_checklist(
                ui,
                "report",
                &self.participants,
                &mut self.report_checked,
                |_| true,
                colors,
            );
        }

        // 2. Delete all messages & reactions
        if self.can_delete_all() {
            ui.add_space(6.0);
            let del_label = if is_single {
                format!("Delete all from {single_name}")
            } else {
                "Delete all messages".to_owned()
            };
            ui.horizontal(|ui| {
                let mut combined = self.delete_all_messages && self.delete_all_reactions;
                if is_single {
                    if Checkbox::new(&mut combined, &del_label, cb_st, check_st)
                        .show(ui)
                        .changed()
                    {
                        self.delete_all_messages = combined;
                        self.delete_all_reactions = combined;
                    }
                    let checked_n = usize::from(self.delete_all_messages)
                        + usize::from(self.delete_all_reactions);
                    if ui.small_button(format!("{checked_n} / 2")).clicked() {
                        self.delete_all_expanded = !self.delete_all_expanded;
                    }
                } else {
                    Checkbox::new(&mut self.delete_all_messages, &del_label, cb_st, check_st)
                        .show(ui);
                    if ui
                        .small_button(format!("{}", self.delete_all_checked.len()))
                        .clicked()
                    {
                        self.delete_all_expanded = !self.delete_all_expanded;
                    }
                }
            });

            if is_single && self.delete_all_expanded {
                ui.indent("moderate_single_del_sub", |ui| {
                    Checkbox::new(
                        &mut self.delete_all_messages,
                        "All messages",
                        cb_st,
                        check_st,
                    )
                    .show(ui);
                    Checkbox::new(
                        &mut self.delete_all_reactions,
                        "All reactions",
                        cb_st,
                        check_st,
                    )
                    .show(ui);
                });
            } else if !is_single {
                if self.delete_all_expanded {
                    render_participant_checklist(
                        ui,
                        "delete_all",
                        &self.participants,
                        &mut self.delete_all_checked,
                        |p| p.can_delete_all,
                        colors,
                    );
                }
                ui.add_space(4.0);
                Checkbox::new(
                    &mut self.delete_all_reactions,
                    "Delete all reactions",
                    cb_st,
                    check_st,
                )
                .show(ui);
            }
        }

        // 3. Ban / Restrict user(s)
        if self.can_ban_any() {
            ui.add_space(6.0);
            let ban_label = if is_single {
                format!("Ban {single_name}")
            } else {
                "Ban users".to_owned()
            };
            ui.horizontal(|ui| {
                Checkbox::new(&mut self.ban_or_restrict, &ban_label, cb_st, check_st).show(ui);
                if !is_single
                    && ui
                        .small_button(format!("{}", self.ban_checked.len()))
                        .clicked()
                {
                    self.ban_expanded = !self.ban_expanded;
                }
            });
            if !is_single && self.ban_expanded {
                render_participant_checklist(
                    ui,
                    "ban",
                    &self.participants,
                    &mut self.ban_checked,
                    |p| p.can_ban,
                    colors,
                );
            }

            if self.ban_or_restrict {
                ui.add_space(4.0);
                ui.indent("moderate_ban_duration", |ui| {
                    ui.horizontal_wrapped(|ui| {
                        for preset in RestrictDurationPreset::ALL {
                            if ui
                                .selectable_label(self.restrict_preset == preset, preset.label())
                                .clicked()
                            {
                                self.restrict_preset = preset;
                            }
                        }
                    });
                    if let Some(ref comm_title) = self.linked_community_title {
                        ui.add_space(4.0);
                        Checkbox::new(
                            &mut self.ban_from_community,
                            &format!("Also ban from community \"{comm_title}\""),
                            cb_st,
                            check_st,
                        )
                        .show(ui);
                    }
                });
            }
        }

        if let Some(ref err) = self.error {
            ui.add_space(8.0);
            divider_bar(ui, 4.0, colors);
            ui.label(
                egui::RichText::new(err)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("boxTextFgError")),
            );
        }
    }
}

fn render_participant_checklist(
    ui: &mut Ui,
    salt: &str,
    participants: &[ModerateParticipant],
    checked: &mut HashSet<PeerId>,
    allowed: impl Fn(&ModerateParticipant) -> bool,
    colors: &dyn Colors,
) {
    ui.indent(("moderate_sublist", salt), |ui| {
        for p in participants {
            if !allowed(p) {
                continue;
            }
            let mut is_on = checked.contains(&p.peer_id);
            let label = match p.total_messages_count {
                Some(total) => format!("{} ({total} msgs)", p.name),
                None => p.name.clone(),
            };
            ui.horizontal(|ui| {
                if ui.checkbox(&mut is_on, "").changed() {
                    if is_on {
                        checked.insert(p.peer_id);
                    } else {
                        checked.remove(&p.peer_id);
                    }
                }
                ui.label(
                    egui::RichText::new(label)
                        .font(FontId::new(12.5, family_semibold()))
                        .color(colors.color("windowFg")),
                );
                if let Some(ref u) = p.username {
                    ui.label(
                        egui::RichText::new(format!("@{u}"))
                            .font(FontId::new(12.0, family_regular()))
                            .color(colors.color("windowSubTextFg")),
                    );
                }
            });
        }
    });
}
