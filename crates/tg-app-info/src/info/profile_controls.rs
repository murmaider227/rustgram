//! WP-63 Peer Profile UI controls:
//! - Personal Channel card & picker (`info_profile_actions.cpp`, `account.updatePersonalChannel`)
//! - Main Profile Tab selector & context menu (`info_profile_tabs_host.cpp`, `{account,channels}.setMainProfileTab`)
//! - Suggest Date of Birth dialog (`users.suggestBirthday`, `local_url_handlers.cpp`)
//! - Report Profile Photo dialog (`account.reportProfilePhoto`, `api_report.cpp`)

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Margin, Pos2, Rect, Role, Sense,
    Stroke, TextEdit, Ui, Vec2, WidgetInfo,
};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold, font_id_from_style};
use tg_ui::icons::paint_style_icon;
use tg_ui::stand_in::Colors;
use tg_ui::tg_style::st;
use tg_ui::theme::{ColorsResolver, resolve_color_ref};
use tg_ui::userpic::EmptyUserpic;

use crate::info::backend::{
    InfoPeerKind, PeerProfile, PersonalChannelOption, ProfileMainTab, ProfilePhotoRef,
    ReportReason, SharedMediaTab,
};
use crate::info::values::{Birthday, MAX_REPORT_COMMENT_LENGTH, format_subscribers_status};
use tg_app_core::core_types::dto::contacts_extras::{
    ContactProfilePhotoMode, ContactProfilePhotoUpload,
};
use tg_app_core::stand_in::types::PeerId;

/// Returns whether `peer` allows setting `tab` as its Main Profile Tab, matching
/// `TabsHost::canSetMainTab` (`info_profile_tabs_host.cpp:283-292`).
#[must_use]
pub fn can_set_main_profile_tab(peer: &PeerProfile, tab: ProfileMainTab) -> bool {
    if peer.kind.is_channel() {
        return peer.can_edit;
    }
    if peer.kind == InfoPeerKind::SavedMessages
        || (peer.kind == InfoPeerKind::User && peer.can_edit)
    {
        return matches!(tab, ProfileMainTab::Posts | ProfileMainTab::Gifts);
    }
    false
}

/// Returns the ordered list of selectable main tabs for `peer`.
#[must_use]
pub fn selectable_main_tabs_for_peer(peer: &PeerProfile) -> Vec<ProfileMainTab> {
    if peer.kind.is_channel() && peer.can_edit {
        vec![
            ProfileMainTab::Posts,
            ProfileMainTab::Gifts,
            ProfileMainTab::Media,
            ProfileMainTab::Files,
            ProfileMainTab::Music,
            ProfileMainTab::Voice,
            ProfileMainTab::Links,
            ProfileMainTab::Gifs,
        ]
    } else if peer.kind == InfoPeerKind::SavedMessages
        || (peer.kind == InfoPeerKind::User && peer.can_edit)
    {
        vec![ProfileMainTab::Posts, ProfileMainTab::Gifts]
    } else {
        Vec::new()
    }
}

/// Maps a [`SharedMediaTab`] to its corresponding [`ProfileMainTab`].
#[must_use]
pub const fn shared_media_to_profile_main_tab(tab: SharedMediaTab) -> ProfileMainTab {
    match tab {
        SharedMediaTab::PhotosVideos => ProfileMainTab::Media,
        SharedMediaTab::Files => ProfileMainTab::Files,
        SharedMediaTab::Links => ProfileMainTab::Links,
        SharedMediaTab::Voice => ProfileMainTab::Voice,
        SharedMediaTab::Music => ProfileMainTab::Music,
        SharedMediaTab::Gifs => ProfileMainTab::Gifs,
    }
}

/// Maps a [`ProfileMainTab`] to its [`SharedMediaTab`] counterpart, if it is a media tab.
#[must_use]
pub const fn profile_main_tab_to_shared_media(tab: ProfileMainTab) -> Option<SharedMediaTab> {
    match tab {
        ProfileMainTab::Media => Some(SharedMediaTab::PhotosVideos),
        ProfileMainTab::Files => Some(SharedMediaTab::Files),
        ProfileMainTab::Links => Some(SharedMediaTab::Links),
        ProfileMainTab::Voice => Some(SharedMediaTab::Voice),
        ProfileMainTab::Music => Some(SharedMediaTab::Music),
        ProfileMainTab::Gifs => Some(SharedMediaTab::Gifs),
        ProfileMainTab::Posts | ProfileMainTab::Gifts => None,
    }
}

/// Returns a concise human-readable label for a [`ProfileMainTab`].
#[must_use]
pub const fn profile_main_tab_label(tab: ProfileMainTab) -> &'static str {
    match tab {
        ProfileMainTab::Posts => "Posts",
        ProfileMainTab::Gifts => "Gifts",
        ProfileMainTab::Media => "Media",
        ProfileMainTab::Files => "Files",
        ProfileMainTab::Music => "Music",
        ProfileMainTab::Voice => "Voice",
        ProfileMainTab::Links => "Links",
        ProfileMainTab::Gifs => "GIFs",
    }
}

/// Returns the ordered [`SharedMediaTab`] slice with `main_tab` rotated to the front when it is
/// one of the shared-media tabs (port of `TabsHost::refreshOrder`, `info_profile_tabs_host.cpp:235-268`).
#[must_use]
pub fn ordered_shared_media_tabs(main_tab: Option<ProfileMainTab>) -> [SharedMediaTab; 6] {
    let mut tabs = SharedMediaTab::ALL;
    if let Some(target) = main_tab.and_then(profile_main_tab_to_shared_media)
        && let Some(pos) = tabs.iter().position(|&t| t == target)
        && pos > 0
    {
        tabs[..=pos].rotate_right(1);
    }
    tabs
}

/// Actions emitted by [`PersonalChannelSection::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersonalChannelAction {
    /// Open the linked personal channel profile/history.
    OpenChannel(PeerId),
    /// Set a new personal channel (`account.updatePersonalChannel`).
    SelectChannel(PeerId),
    /// Clear the personal channel (`account.updatePersonalChannel(InputChannel::Empty)`).
    RemoveChannel,
}

/// Renders the peer's Personal Channel block and (for self profile) the Personal Channel picker
/// (`info_profile_actions.cpp:1915-2250`, `account.updatePersonalChannel`).
pub struct PersonalChannelSection;

impl PersonalChannelSection {
    /// Renders the Personal Channel row and optional inline picker sheet.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        ui: &mut Ui,
        peer: &PeerProfile,
        current: Option<&PersonalChannelOption>,
        candidates: &[PersonalChannelOption],
        picker_open: &mut bool,
        colors: &dyn Colors,
    ) -> Option<PersonalChannelAction> {
        let is_self_editable = peer.kind == InfoPeerKind::SavedMessages;
        if current.is_none() && !is_self_editable {
            return None;
        }

        let mut action = None;
        let width = ui.available_width().max(200.0);

        if let Some(ch) = current {
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 54.0), Sense::click());
            let acc_name = format!("{}: {}", tr::lng_info_personal_channel_label(), ch.title);
            resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, &acc_name));
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                ui.painter()
                    .rect_filled(rect, CornerRadius::ZERO, colors.color("windowBgOver"));
            }

            // Channel avatar (`infoPersonalChannelUserpic`, 36px circle).
            let av_rect = Rect::from_center_size(
                Pos2::new(rect.left() + 30.0, rect.center().y),
                Vec2::splat(34.0),
            );
            EmptyUserpic::for_color_index((ch.peer_id.bare() % 7) as u8, &ch.title, colors)
                .paint_circle_or_photo(ui.painter(), av_rect, ch.peer_id.0);

            let text_x = rect.left() + 56.0;
            let right_reserve = if is_self_editable { 116.0 } else { 26.0 };
            let max_text_w = (rect.right() - right_reserve - text_x).max(60.0);
            let title_font = font_id_from_style(st::semibold_font);
            let sub_font = FontId::new(11.5, family_regular());
            let title_display = ellipsize_to_width(ui, &ch.title, &title_font, max_text_w);
            let sub_raw = format!(
                "{} · {}",
                tr::lng_info_personal_channel_label(),
                format_subscribers_status(ch.subscribers_count)
            );
            let sub_display = ellipsize_to_width(ui, &sub_raw, &sub_font, max_text_w);

            ui.painter().text(
                Pos2::new(text_x, rect.top() + 8.0),
                Align2::LEFT_TOP,
                title_display,
                title_font,
                colors.color("windowFg"),
            );
            ui.painter().text(
                Pos2::new(text_x, rect.top() + 28.0),
                Align2::LEFT_TOP,
                sub_display,
                sub_font,
                colors.color("windowSubTextFg"),
            );

            if !is_self_editable {
                ui.painter().text(
                    Pos2::new(rect.right() - 16.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    "›",
                    FontId::new(16.0, family_regular()),
                    colors.color("windowSubTextFg"),
                );
            }

            let mut button_hit = false;
            if is_self_editable {
                let change_rect = Rect::from_center_size(
                    Pos2::new(rect.right() - 78.0, rect.center().y),
                    Vec2::new(56.0, 26.0),
                );
                let ch_resp = ui.interact(
                    change_rect,
                    ui.make_persistent_id(("personal_ch_change", peer.id.0)),
                    Sense::click(),
                );
                ch_resp.widget_info(|| {
                    WidgetInfo::labeled(Role::Button, true, "Change Personal Channel")
                });
                if ch_resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                ui.painter().rect_filled(
                    change_rect,
                    CornerRadius::same(13),
                    colors.color("windowBgOver"),
                );
                ui.painter().text(
                    change_rect.center(),
                    Align2::CENTER_CENTER,
                    "Change",
                    FontId::new(11.0, family_semibold()),
                    colors.color("windowActiveTextFg"),
                );
                if ch_resp.clicked() {
                    *picker_open = !*picker_open;
                    button_hit = true;
                }

                let rm_rect = Rect::from_center_size(
                    Pos2::new(rect.right() - 26.0, rect.center().y),
                    Vec2::new(38.0, 26.0),
                );
                let rm_resp = ui.interact(
                    rm_rect,
                    ui.make_persistent_id(("personal_ch_remove", peer.id.0)),
                    Sense::click(),
                );
                rm_resp.widget_info(|| {
                    WidgetInfo::labeled(Role::Button, true, tr::lng_settings_channel_menu_remove())
                });
                if rm_resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                ui.painter().rect_filled(
                    rm_rect,
                    CornerRadius::same(13),
                    colors.color("windowBgOver"),
                );
                ui.painter().text(
                    rm_rect.center(),
                    Align2::CENTER_CENTER,
                    "×",
                    FontId::new(13.0, family_semibold()),
                    colors.color("attentionButtonFg"),
                );
                if rm_resp.clicked() {
                    *picker_open = false;
                    action = Some(PersonalChannelAction::RemoveChannel);
                    button_hit = true;
                }
            }

            if resp.clicked() && !button_hit {
                action = Some(PersonalChannelAction::OpenChannel(ch.peer_id));
            }
        } else if is_self_editable {
            // Self profile with no personal channel linked yet: compact "Personal channel · Add" row.
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 42.0), Sense::click());
            let lbl = tr::lng_settings_channel_label();
            resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, &lbl));
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                ui.painter()
                    .rect_filled(rect, CornerRadius::ZERO, colors.color("windowBgOver"));
            }
            let icon_rect = Rect::from_center_size(
                Pos2::new(rect.left() + 30.0, rect.center().y),
                Vec2::splat(20.0),
            );
            paint_style_icon(
                ui.painter(),
                icon_rect,
                st::info_icon_media_group,
                &ColorsResolver(colors),
                Some(resolve_color_ref(st::info_icon_fg, colors)),
            );
            ui.painter().text(
                Pos2::new(rect.left() + 56.0, rect.center().y),
                Align2::LEFT_CENTER,
                &lbl,
                font_id_from_style(st::info_profile_button.style.font),
                colors.color("windowFg"),
            );
            ui.painter().text(
                Pos2::new(rect.right() - 18.0, rect.center().y),
                Align2::RIGHT_CENTER,
                tr::lng_settings_channel_add(),
                FontId::new(12.5, family_semibold()),
                colors.color("windowActiveTextFg"),
            );
            if resp.clicked() {
                *picker_open = !*picker_open;
            }
        }

        // Inline Personal Channel Picker card (`account.updatePersonalChannel`)
        if *picker_open && is_self_editable {
            Frame::NONE
                .inner_margin(Margin::symmetric(12, 4))
                .show(ui, |ui| {
                    Frame::NONE
                        .fill(colors.color("windowBgOver"))
                        .stroke(Stroke::new(1.0, colors.color("shadowFg")))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::same(10))
                        .show(ui, |ui| {
                            ui.set_min_width((width - 44.0).max(160.0));
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(tr::lng_settings_channel_label())
                                        .font(font_id_from_style(st::semibold_font))
                                        .color(colors.color("windowBoldFg")),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.small_button(tr::lng_close()).clicked() {
                                            *picker_open = false;
                                        }
                                    },
                                );
                            });
                            ui.add_space(4.0);
                            if candidates.is_empty() {
                                ui.label(
                                    egui::RichText::new(tr::lng_settings_channel_no_yet())
                                        .font(font_id_from_style(st::normal_font))
                                        .color(colors.color("windowSubTextFg")),
                                );
                            } else {
                                for cand in candidates {
                                    let is_selected =
                                        current.is_some_and(|c| c.peer_id == cand.peer_id);
                                    let row_w = ui.available_width().max(150.0);
                                    let (r, r_resp) = ui.allocate_exact_size(
                                        Vec2::new(row_w, 36.0),
                                        Sense::click(),
                                    );
                                    let row_label = format!(
                                        "{} ({})",
                                        cand.title,
                                        format_subscribers_status(cand.subscribers_count)
                                    );
                                    r_resp.widget_info(|| {
                                        WidgetInfo::selected(
                                            Role::Button,
                                            true,
                                            is_selected,
                                            &row_label,
                                        )
                                    });
                                    if r_resp.hovered() {
                                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                                    }
                                    let fill = if is_selected {
                                        colors.color("windowBgActive")
                                    } else if r_resp.hovered() {
                                        colors.color("windowBg")
                                    } else {
                                        Color32::TRANSPARENT
                                    };
                                    ui.painter().rect_filled(r, CornerRadius::same(6), fill);
                                    let fg = if is_selected {
                                        Color32::WHITE
                                    } else {
                                        colors.color("windowFg")
                                    };
                                    let sub_fg = if is_selected {
                                        Color32::from_rgba_unmultiplied(255, 255, 255, 220)
                                    } else {
                                        colors.color("windowSubTextFg")
                                    };
                                    ui.painter().text(
                                        Pos2::new(r.left() + 10.0, r.center().y),
                                        Align2::LEFT_CENTER,
                                        &cand.title,
                                        font_id_from_style(st::semibold_font),
                                        fg,
                                    );
                                    let right_meta = cand
                                        .username
                                        .as_deref()
                                        .map(|u| format!("@{u}"))
                                        .unwrap_or_else(|| {
                                            format_subscribers_status(cand.subscribers_count)
                                        });
                                    ui.painter().text(
                                        Pos2::new(r.right() - 10.0, r.center().y),
                                        Align2::RIGHT_CENTER,
                                        right_meta,
                                        FontId::new(11.0, family_regular()),
                                        sub_fg,
                                    );
                                    if r_resp.clicked() {
                                        *picker_open = false;
                                        action = Some(PersonalChannelAction::SelectChannel(
                                            cand.peer_id,
                                        ));
                                    }
                                }
                            }
                            if current.is_some() {
                                ui.add_space(6.0);
                                let rm_btn = egui::Button::new(
                                    egui::RichText::new(tr::lng_settings_channel_menu_remove())
                                        .color(colors.color("attentionButtonFg")),
                                );
                                if ui.add(rm_btn).clicked() {
                                    *picker_open = false;
                                    action = Some(PersonalChannelAction::RemoveChannel);
                                }
                            }
                        });
                });
        }

        action
    }
}

/// Renders the accessible Main Profile Tab selector bar above the profile media tabs
/// (`info_profile_tabs_host.cpp`). Returns `Some(ProfileMainTab)` when the user picks a new main tab.
pub fn show_main_profile_tab_bar(
    ui: &mut Ui,
    peer: &PeerProfile,
    current_main: Option<ProfileMainTab>,
    colors: &dyn Colors,
) -> Option<ProfileMainTab> {
    let options = selectable_main_tabs_for_peer(peer);
    if options.is_empty() {
        return None;
    }

    let mut chosen = None;
    Frame::NONE
        .inner_margin(Margin::symmetric(12, 4))
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(6.0, 4.0);
                ui.label(
                    egui::RichText::new(format!("{}:", tr::lng_profile_tab_set_as_main()))
                        .font(FontId::new(11.5, family_semibold()))
                        .color(colors.color("windowSubTextFg")),
                );
                for tab in options {
                    let is_main = current_main == Some(tab);
                    let lbl = profile_main_tab_label(tab).to_owned();
                    let galley = ui.painter().layout_no_wrap(
                        lbl,
                        FontId::new(11.0, family_semibold()),
                        if is_main {
                            Color32::WHITE
                        } else {
                            colors.color("windowFg")
                        },
                    );
                    let star_reserve = if is_main { 14.0 } else { 0.0 };
                    let (rect, resp) = ui.allocate_exact_size(
                        Vec2::new(galley.size().x + 16.0 + star_reserve, 24.0),
                        Sense::click(),
                    );
                    let acc = format!(
                        "{}: {}",
                        tr::lng_profile_tab_set_as_main(),
                        profile_main_tab_label(tab)
                    );
                    resp.widget_info(|| WidgetInfo::selected(Role::Button, true, is_main, &acc));
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    let bg = if is_main {
                        colors.color("windowBgActive")
                    } else if resp.hovered() {
                        colors.color("windowBgOver")
                    } else {
                        Color32::from_rgba_unmultiplied(128, 128, 128, 24)
                    };
                    ui.painter().rect_filled(rect, CornerRadius::same(12), bg);
                    let content_w = star_reserve + galley.size().x;
                    let left_x = rect.center().x - content_w * 0.5;
                    if is_main {
                        tg_app_core::ui_icons::paint_star(
                            ui.painter(),
                            Pos2::new(left_x + 5.0, rect.center().y),
                            4.5,
                            Color32::WHITE,
                        );
                    }
                    ui.painter().galley(
                        Pos2::new(
                            left_x + star_reserve,
                            rect.center().y - galley.size().y * 0.5,
                        ),
                        galley,
                        Color32::WHITE,
                    );
                    if resp.clicked() && !is_main {
                        chosen = Some(tab);
                    }
                }
            });
        });

    chosen
}

/// Inline form state for suggesting a contact's birthday (`users.suggestBirthday`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuggestBirthdayDraft {
    /// Target user peer ID.
    pub peer_id: PeerId,
    /// Target user display title.
    pub peer_title: String,
    /// Day input (`1..=31`).
    pub day_input: String,
    /// Month input (`1..=12`).
    pub month_input: String,
    /// Optional birth year (`1900..=2026`).
    pub year_input: String,
    /// Inline validation error.
    pub error: Option<String>,
}

impl SuggestBirthdayDraft {
    /// Creates a new draft pre-filled with a sensible example date.
    #[must_use]
    pub fn new(peer_id: PeerId, peer_title: impl Into<String>) -> Self {
        Self {
            peer_id,
            peer_title: peer_title.into(),
            day_input: "15".to_owned(),
            month_input: "6".to_owned(),
            year_input: String::new(),
            error: None,
        }
    }

    /// Parses and validates the entered day/month/year into a [`Birthday`].
    pub fn parse_birthday(&self) -> Result<Birthday, String> {
        let day: u8 = self
            .day_input
            .trim()
            .parse()
            .map_err(|_| "Enter a day between 1 and 31.".to_owned())?;
        let month: u8 = self
            .month_input
            .trim()
            .parse()
            .map_err(|_| "Enter a month between 1 and 12.".to_owned())?;
        if !(1..=12).contains(&month) {
            return Err("Month must be between 1 and 12.".to_owned());
        }
        let max_day = match month {
            2 => 29,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        };
        if !(1..=max_day).contains(&day) {
            return Err(format!(
                "Day must be between 1 and {max_day} for month {month}."
            ));
        }
        let year = if self.year_input.trim().is_empty() {
            None
        } else {
            let y: u16 = self
                .year_input
                .trim()
                .parse()
                .map_err(|_| "Enter a 4-digit year (e.g. 1995) or leave empty.".to_owned())?;
            if !(1900..=2026).contains(&y) {
                return Err("Year must be between 1900 and 2026.".to_owned());
            }
            Some(y)
        };
        Ok(Birthday { day, month, year })
    }
}

/// Outcome emitted by [`show_suggest_birthday_card`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuggestBirthdayOutcome {
    /// User submitted a valid birthday suggestion.
    Submit(Birthday),
    /// User cancelled.
    Cancel,
}

/// Renders the inline Suggest Date of Birth card (`users.suggestBirthday`).
pub fn show_suggest_birthday_card(
    ui: &mut Ui,
    draft: &mut SuggestBirthdayDraft,
    colors: &dyn Colors,
) -> Option<SuggestBirthdayOutcome> {
    let mut outcome = None;
    let width = ui.available_width().max(200.0);
    Frame::NONE
        .inner_margin(Margin::symmetric(12, 6))
        .show(ui, |ui| {
            Frame::NONE
                .fill(colors.color("windowBgOver"))
                .stroke(Stroke::new(1.0, colors.color("windowBgActive")))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(12))
                .show(ui, |ui| {
                    ui.set_min_width((width - 48.0).max(160.0));
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(tr::lng_suggest_birthday())
                                .font(font_id_from_style(st::semibold_font))
                                .color(colors.color("windowBoldFg")),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button(tr::lng_cancel()).clicked() {
                                outcome = Some(SuggestBirthdayOutcome::Cancel);
                            }
                        });
                    });
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "Suggest a birthday for {} to display on their profile.",
                            draft.peer_title
                        ))
                        .font(FontId::new(11.5, family_regular()))
                        .color(colors.color("windowSubTextFg")),
                    );
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Day");
                        ui.add(
                            TextEdit::singleline(&mut draft.day_input)
                                .desired_width(42.0)
                                .hint_text("15"),
                        );
                        ui.label("Month");
                        ui.add(
                            TextEdit::singleline(&mut draft.month_input)
                                .desired_width(42.0)
                                .hint_text("6"),
                        );
                        ui.label("Year");
                        ui.add(
                            TextEdit::singleline(&mut draft.year_input)
                                .desired_width(58.0)
                                .hint_text("Optional"),
                        );
                    });
                    if let Some(ref err) = draft.error {
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(err)
                                .font(FontId::new(11.5, family_regular()))
                                .color(colors.color("boxTextFgError")),
                        );
                    }
                    ui.add_space(8.0);
                    let submit_lbl = tr::lng_suggest_birthday_box_confirm();
                    if ui.button(&submit_lbl).clicked() {
                        match draft.parse_birthday() {
                            Ok(bday) => {
                                draft.error = None;
                                outcome = Some(SuggestBirthdayOutcome::Submit(bday));
                            }
                            Err(err) => {
                                draft.error = Some(err);
                            }
                        }
                    }
                });
        });
    outcome
}

/// Returns `true` when `photo` carries a real server `id` and `access_hash`
/// (rejecting placeholder `id == 0` or `access_hash == 0` refs before any RPC).
#[must_use]
pub const fn is_valid_profile_photo_ref(photo: &ProfilePhotoRef) -> bool {
    photo.id != 0 && photo.access_hash != 0
}

/// Inline form state for reporting a peer's profile photo (`account.reportProfilePhoto`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportProfilePhotoDraft {
    /// Target peer ID.
    pub peer_id: PeerId,
    /// Target peer title.
    pub peer_title: String,
    /// Real server profile photo reference (`id`, `access_hash`, `file_reference`).
    pub photo: ProfilePhotoRef,
    /// Selected abuse reason.
    pub reason: ReportReason,
    /// Optional report comment.
    pub comment: String,
    /// Inline validation error.
    pub error: Option<String>,
}

impl ReportProfilePhotoDraft {
    /// Creates a new profile-photo report draft bound to a real [`ProfilePhotoRef`].
    #[must_use]
    pub fn new(peer_id: PeerId, peer_title: impl Into<String>, photo: ProfilePhotoRef) -> Self {
        Self {
            peer_id,
            peer_title: peer_title.into(),
            photo,
            reason: ReportReason::Spam,
            comment: String::new(),
            error: None,
        }
    }
}

/// Outcome emitted by [`show_report_profile_photo_card`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportProfilePhotoOutcome {
    /// Submit `account.reportProfilePhoto`.
    Submit {
        /// Selected reason.
        reason: ReportReason,
        /// Optional comment.
        comment: String,
    },
    /// User cancelled.
    Cancel,
}

/// Renders the inline Report Profile Photo confirmation card (`account.reportProfilePhoto`).
pub fn show_report_profile_photo_card(
    ui: &mut Ui,
    draft: &mut ReportProfilePhotoDraft,
    colors: &dyn Colors,
) -> Option<ReportProfilePhotoOutcome> {
    let mut outcome = None;
    let width = ui.available_width().max(200.0);
    Frame::NONE
        .inner_margin(Margin::symmetric(12, 6))
        .show(ui, |ui| {
            Frame::NONE
                .fill(colors.color("windowBgOver"))
                .stroke(Stroke::new(1.0, colors.color("attentionButtonFg")))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(12))
                .show(ui, |ui| {
                    ui.set_min_width((width - 48.0).max(160.0));
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Report Profile Photo")
                                .font(font_id_from_style(st::semibold_font))
                                .color(colors.color("windowBoldFg")),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button(tr::lng_cancel()).clicked() {
                                outcome = Some(ReportProfilePhotoOutcome::Cancel);
                            }
                        });
                    });
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "Choose why {}'s profile photo should be reviewed:",
                            draft.peer_title
                        ))
                        .font(FontId::new(11.5, family_regular()))
                        .color(colors.color("windowSubTextFg")),
                    );
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        for reason in [
                            ReportReason::Spam,
                            ReportReason::Violence,
                            ReportReason::Pornography,
                            ReportReason::Fake,
                            ReportReason::Other,
                        ] {
                            let selected = draft.reason == reason;
                            if ui
                                .selectable_label(selected, reason.localized_label())
                                .clicked()
                            {
                                draft.reason = reason;
                                draft.error = None;
                            }
                        }
                    });
                    ui.add_space(6.0);
                    ui.add(
                        TextEdit::singleline(&mut draft.comment)
                            .desired_width(ui.available_width())
                            .hint_text("Optional details"),
                    );
                    if draft.comment.chars().count() > MAX_REPORT_COMMENT_LENGTH {
                        draft.comment = draft
                            .comment
                            .chars()
                            .take(MAX_REPORT_COMMENT_LENGTH)
                            .collect();
                    }
                    if let Some(ref err) = draft.error {
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(err)
                                .font(FontId::new(11.5, family_regular()))
                                .color(colors.color("boxTextFgError")),
                        );
                    }
                    ui.add_space(8.0);
                    let submit_btn = egui::Button::new(
                        egui::RichText::new("Submit Photo Report")
                            .color(colors.color("attentionButtonFg")),
                    );
                    if ui.add(submit_btn).clicked() {
                        if draft.reason == ReportReason::Other && draft.comment.trim().is_empty() {
                            draft.error = Some(
                                "Describe the issue before submitting an Other report.".to_owned(),
                            );
                        } else {
                            draft.error = None;
                            outcome = Some(ReportProfilePhotoOutcome::Submit {
                                reason: draft.reason,
                                comment: draft.comment.trim().to_owned(),
                            });
                        }
                    }
                });
        });
    outcome
}

/// Maximum private contact note length (`contact_note_length_limit`, `edit_contact_box.cpp`).
pub const MAX_CONTACT_NOTE_LENGTH: usize = 128;

/// Inline form state for editing a private Contact Note (`contacts.updateContactNote`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactNoteDraft {
    /// Target contact peer ID.
    pub peer_id: PeerId,
    /// Target contact display title.
    pub peer_title: String,
    /// Draft private note text.
    pub text: String,
    /// Whether an existing note was loaded (enables Delete Note button).
    pub had_existing: bool,
    /// Inline error message.
    pub error: Option<String>,
}

impl ContactNoteDraft {
    /// Creates a new Contact Note editor draft initialized with `existing_note`.
    #[must_use]
    pub fn new(
        peer_id: PeerId,
        peer_title: impl Into<String>,
        existing_note: Option<&str>,
    ) -> Self {
        let initial = existing_note.unwrap_or_default().to_owned();
        let had_existing = !initial.trim().is_empty();
        Self {
            peer_id,
            peer_title: peer_title.into(),
            text: initial,
            had_existing,
            error: None,
        }
    }
}

/// Outcome emitted by [`show_contact_note_card`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContactNoteOutcome {
    /// Save updated private contact note (`contacts.updateContactNote`).
    Save(String),
    /// Clear/delete the private contact note (`contacts.updateContactNote` with empty text).
    Delete,
    /// Cancel editing.
    Cancel,
}

/// Renders the inline Contact Note editor card (`info_profile_actions.cpp:789-900`,
/// `edit_contact_box.cpp:963`).
pub fn show_contact_note_card(
    ui: &mut Ui,
    draft: &mut ContactNoteDraft,
    colors: &dyn Colors,
) -> Option<ContactNoteOutcome> {
    let mut outcome = None;
    let width = ui.available_width().max(200.0);
    Frame::NONE
        .inner_margin(Margin::symmetric(12, 6))
        .show(ui, |ui| {
            Frame::NONE
                .fill(colors.color("windowBgOver"))
                .stroke(Stroke::new(1.0, colors.color("windowBgActive")))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(12))
                .show(ui, |ui| {
                    ui.set_min_width((width - 48.0).max(160.0));
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Contact Note")
                                .font(font_id_from_style(st::semibold_font))
                                .color(colors.color("windowBoldFg")),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button(tr::lng_cancel()).clicked() {
                                outcome = Some(ContactNoteOutcome::Cancel);
                            }
                            ui.label(
                                egui::RichText::new(format!(
                                    "{}/{MAX_CONTACT_NOTE_LENGTH}",
                                    draft.text.chars().count()
                                ))
                                .font(FontId::new(11.0, family_regular()))
                                .color(colors.color("windowSubTextFg")),
                            );
                        });
                    });
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "Private note for {} (only visible to you).",
                            draft.peer_title
                        ))
                        .font(FontId::new(11.5, family_regular()))
                        .color(colors.color("windowSubTextFg")),
                    );
                    ui.add_space(6.0);
                    ui.add(
                        TextEdit::multiline(&mut draft.text)
                            .desired_width(ui.available_width())
                            .desired_rows(2)
                            .hint_text("Add a private note..."),
                    );
                    let char_count = draft.text.chars().count();
                    if char_count > MAX_CONTACT_NOTE_LENGTH {
                        let excess = (char_count - MAX_CONTACT_NOTE_LENGTH) as i64;
                        draft.error = Some(tr::lng_contact_notes_limit_reached(excess));
                    } else {
                        draft.error = None;
                    }
                    if let Some(ref err) = draft.error {
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(err)
                                .font(FontId::new(11.5, family_regular()))
                                .color(colors.color("boxTextFgError")),
                        );
                    }
                    ui.add_space(8.0);
                    ui.horizontal_wrapped(|ui| {
                        let can_save = draft.text.chars().count() <= MAX_CONTACT_NOTE_LENGTH;
                        if ui
                            .add_enabled(can_save, egui::Button::new("Save Note"))
                            .clicked()
                        {
                            outcome = Some(ContactNoteOutcome::Save(draft.text.trim().to_owned()));
                        }
                        if draft.had_existing {
                            let del_btn = egui::Button::new(
                                egui::RichText::new("Delete Note")
                                    .color(colors.color("attentionButtonFg")),
                            );
                            if ui.add(del_btn).clicked() {
                                outcome = Some(ContactNoteOutcome::Delete);
                            }
                        }
                    });
                });
        });
    outcome
}

/// Inline confirmation state for deleting a contact (`contacts.deleteContacts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteContactDraft {
    /// Target contact peer ID.
    pub peer_id: PeerId,
    /// Target contact display title.
    pub peer_title: String,
    /// Inline error message, if RPC fails.
    pub error: Option<String>,
}

/// Outcome emitted by [`show_delete_contact_card`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteContactOutcome {
    /// User confirmed deleting the contact.
    Confirm,
    /// User cancelled.
    Cancel,
}

/// Renders the inline Delete Contact confirmation card (`info_profile_actions.cpp:2983`).
pub fn show_delete_contact_card(
    ui: &mut Ui,
    draft: &DeleteContactDraft,
    colors: &dyn Colors,
) -> Option<DeleteContactOutcome> {
    let mut outcome = None;
    let width = ui.available_width().max(200.0);
    Frame::NONE
        .inner_margin(Margin::symmetric(12, 6))
        .show(ui, |ui| {
            Frame::NONE
                .fill(colors.color("windowBgOver"))
                .stroke(Stroke::new(1.0, colors.color("attentionButtonFg")))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(12))
                .show(ui, |ui| {
                    ui.set_min_width((width - 48.0).max(160.0));
                    ui.label(
                        egui::RichText::new(tr::lng_info_delete_contact())
                            .font(font_id_from_style(st::semibold_font))
                            .color(colors.color("windowBoldFg")),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(tr::lng_sure_delete_contact(&draft.peer_title))
                            .font(FontId::new(12.0, family_regular()))
                            .color(colors.color("windowFg")),
                    );
                    if let Some(ref err) = draft.error {
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(err)
                                .font(FontId::new(11.5, family_regular()))
                                .color(colors.color("boxTextFgError")),
                        );
                    }
                    ui.add_space(8.0);
                    ui.horizontal_wrapped(|ui| {
                        let confirm_btn = egui::Button::new(
                            egui::RichText::new("Confirm Delete Contact")
                                .color(colors.color("attentionButtonFg")),
                        );
                        if ui.add(confirm_btn).clicked() {
                            outcome = Some(DeleteContactOutcome::Confirm);
                        }
                        if ui.button(tr::lng_cancel()).clicked() {
                            outcome = Some(DeleteContactOutcome::Cancel);
                        }
                    });
                });
        });
    outcome
}

/// Inline confirmation state for deleting the current account's own profile photo
/// (`photos.deletePhotos`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteOwnPhotoDraft {
    /// Real server profile photo reference (`id`, `access_hash`, `file_reference`).
    pub photo: ProfilePhotoRef,
    /// Inline error message, if RPC fails.
    pub error: Option<String>,
}

/// Outcome emitted by [`show_delete_own_photo_card`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteOwnPhotoOutcome {
    /// Confirm deleting the photo via `photos.deletePhotos`.
    Confirm,
    /// Cancel.
    Cancel,
}

/// Renders the inline Delete Own Profile Photo confirmation card (`photos.deletePhotos`).
pub fn show_delete_own_photo_card(
    ui: &mut Ui,
    draft: &DeleteOwnPhotoDraft,
    colors: &dyn Colors,
) -> Option<DeleteOwnPhotoOutcome> {
    let mut outcome = None;
    let width = ui.available_width().max(200.0);
    Frame::NONE
        .inner_margin(Margin::symmetric(12, 6))
        .show(ui, |ui| {
            Frame::NONE
                .fill(colors.color("windowBgOver"))
                .stroke(Stroke::new(1.0, colors.color("attentionButtonFg")))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(12))
                .show(ui, |ui| {
                    ui.set_min_width((width - 48.0).max(160.0));
                    ui.label(
                        egui::RichText::new("Delete Profile Photo")
                            .font(font_id_from_style(st::semibold_font))
                            .color(colors.color("windowBoldFg")),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(tr::lng_delete_photo_sure())
                            .font(FontId::new(12.0, family_regular()))
                            .color(colors.color("windowFg")),
                    );
                    if let Some(ref err) = draft.error {
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(err)
                                .font(FontId::new(11.5, family_regular()))
                                .color(colors.color("boxTextFgError")),
                        );
                    }
                    ui.add_space(8.0);
                    ui.horizontal_wrapped(|ui| {
                        let del_btn = egui::Button::new(
                            egui::RichText::new("Confirm Delete Photo")
                                .color(colors.color("attentionButtonFg")),
                        );
                        if ui.add(del_btn).clicked() {
                            outcome = Some(DeleteOwnPhotoOutcome::Confirm);
                        }
                        if ui.button(tr::lng_cancel()).clicked() {
                            outcome = Some(DeleteOwnPhotoOutcome::Cancel);
                        }
                    });
                });
        });
    outcome
}

/// Inline form state for suggesting or setting a personal profile photo for a contact
/// (`photos.uploadContactProfilePhoto`, `edit_contact_box.cpp:870-920`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactPhotoDraft {
    /// Target contact peer ID.
    pub peer_id: PeerId,
    /// Target contact display name.
    pub peer_title: String,
    /// Selected upload mode (`Suggest` vs personal `Save`).
    pub mode: ContactProfilePhotoMode,
    /// Inline error message.
    pub error: Option<String>,
}

impl ContactPhotoDraft {
    /// Creates a new Contact Photo draft defaulting to `Suggest`.
    #[must_use]
    pub fn new(
        peer_id: PeerId,
        peer_title: impl Into<String>,
        mode: ContactProfilePhotoMode,
    ) -> Self {
        Self {
            peer_id,
            peer_title: peer_title.into(),
            mode,
            error: None,
        }
    }

    /// Builds a deterministic synthetic JPEG/PNG avatar payload for offline / fake-RPC submission.
    #[must_use]
    pub fn build_upload_request(&self) -> ContactProfilePhotoUpload {
        let filename = match self.mode {
            ContactProfilePhotoMode::Suggest => {
                format!("suggest_photo_{}.jpg", self.peer_id.bare())
            }
            ContactProfilePhotoMode::Save => {
                format!("personal_photo_{}.jpg", self.peer_id.bare())
            }
        };
        // Minimal valid JPEG SOI/EOI marker bytes for deterministic offline/fake tests.
        let bytes = vec![
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0xFF, 0xD9,
        ];
        ContactProfilePhotoUpload::static_photo(self.mode, filename, bytes)
    }
}

/// Outcome emitted by [`show_contact_photo_card`].
#[derive(Debug, Clone, PartialEq)]
pub enum ContactPhotoOutcome {
    /// Upload a suggested or personal photo (`photos.uploadContactProfilePhoto`).
    Upload(ContactProfilePhotoUpload),
    /// Clear the personal contact photo (`PeerPhoto::clearPersonal`).
    ClearPersonal,
    /// Cancel.
    Cancel,
}

/// Renders the inline Suggest / Set Personal Contact Photo card (`photos.uploadContactProfilePhoto`).
pub fn show_contact_photo_card(
    ui: &mut Ui,
    draft: &mut ContactPhotoDraft,
    colors: &dyn Colors,
) -> Option<ContactPhotoOutcome> {
    let mut outcome = None;
    let width = ui.available_width().max(200.0);
    Frame::NONE
        .inner_margin(Margin::symmetric(12, 6))
        .show(ui, |ui| {
            Frame::NONE
                .fill(colors.color("windowBgOver"))
                .stroke(Stroke::new(1.0, colors.color("windowBgActive")))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(12))
                .show(ui, |ui| {
                    ui.set_min_width((width - 48.0).max(160.0));
                    ui.horizontal(|ui| {
                        let heading = match draft.mode {
                            ContactProfilePhotoMode::Suggest => {
                                tr::lng_suggest_photo_for(&draft.peer_title)
                            }
                            ContactProfilePhotoMode::Save => {
                                tr::lng_set_photo_for_user(&draft.peer_title)
                            }
                        };
                        ui.label(
                            egui::RichText::new(heading)
                                .font(font_id_from_style(st::semibold_font))
                                .color(colors.color("windowBoldFg")),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button(tr::lng_cancel()).clicked() {
                                outcome = Some(ContactPhotoOutcome::Cancel);
                            }
                        });
                    });
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .selectable_label(
                                draft.mode == ContactProfilePhotoMode::Suggest,
                                tr::lng_profile_suggest_photo(),
                            )
                            .clicked()
                        {
                            draft.mode = ContactProfilePhotoMode::Suggest;
                        }
                        if ui
                            .selectable_label(
                                draft.mode == ContactProfilePhotoMode::Save,
                                tr::lng_profile_set_photo_for(),
                            )
                            .clicked()
                        {
                            draft.mode = ContactProfilePhotoMode::Save;
                        }
                    });
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(match draft.mode {
                            ContactProfilePhotoMode::Suggest => format!(
                                "{} will receive this photo as a profile photo suggestion.",
                                draft.peer_title
                            ),
                            ContactProfilePhotoMode::Save => format!(
                                "Only you will see this custom photo for {}.",
                                draft.peer_title
                            ),
                        })
                        .font(FontId::new(11.5, family_regular()))
                        .color(colors.color("windowSubTextFg")),
                    );
                    if let Some(ref err) = draft.error {
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(err)
                                .font(FontId::new(11.5, family_regular()))
                                .color(colors.color("boxTextFgError")),
                        );
                    }
                    ui.add_space(8.0);
                    ui.horizontal_wrapped(|ui| {
                        let primary_lbl = match draft.mode {
                            ContactProfilePhotoMode::Suggest => "Send Photo Suggestion",
                            ContactProfilePhotoMode::Save => "Set Personal Photo",
                        };
                        if ui.button(primary_lbl).clicked() {
                            outcome =
                                Some(ContactPhotoOutcome::Upload(draft.build_upload_request()));
                        }
                        let clear_btn = egui::Button::new(
                            egui::RichText::new("Reset Personal Photo")
                                .color(colors.color("attentionButtonFg")),
                        );
                        if ui.add(clear_btn).clicked() {
                            outcome = Some(ContactPhotoOutcome::ClearPersonal);
                        }
                    });
                });
        });
    outcome
}

/// Truncates `text` with a trailing `…` if its single-line layout exceeds `max_width`.
fn ellipsize_to_width(ui: &Ui, text: &str, font: &FontId, max_width: f32) -> String {
    let fits = |candidate: &str| {
        ui.painter()
            .layout_no_wrap(candidate.to_owned(), font.clone(), Color32::WHITE)
            .size()
            .x
            <= max_width
    };
    if fits(text) {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut lo = 0usize;
    let mut hi = chars.len();
    while lo < hi {
        let mid = lo.midpoint(hi + 1);
        let mut s: String = chars[..mid].iter().collect();
        s.push('…');
        if fits(&s) {
            lo = mid;
        } else {
            hi = mid.saturating_sub(1);
        }
    }
    let mut out: String = chars[..lo].iter().collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_shared_media_tabs_rotates_main_tab_first() {
        let default_order = ordered_shared_media_tabs(None);
        assert_eq!(default_order[0], SharedMediaTab::PhotosVideos);

        let files_first = ordered_shared_media_tabs(Some(ProfileMainTab::Files));
        assert_eq!(files_first[0], SharedMediaTab::Files);
        assert_eq!(files_first[1], SharedMediaTab::PhotosVideos);
        assert_eq!(files_first.len(), 6);
    }

    #[test]
    fn suggest_birthday_validates_calendar_bounds() {
        let mut draft = SuggestBirthdayDraft::new(PeerId::from_user(1001), "Mira");
        draft.day_input = "31".to_owned();
        draft.month_input = "2".to_owned();
        assert!(draft.parse_birthday().is_err());

        draft.day_input = "24".to_owned();
        draft.month_input = "11".to_owned();
        draft.year_input = "1996".to_owned();
        assert_eq!(
            draft.parse_birthday().unwrap(),
            Birthday {
                day: 24,
                month: 11,
                year: Some(1996)
            }
        );
    }
}
