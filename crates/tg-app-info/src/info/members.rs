//! Group/channel members list with admin badges & search, plus common groups list.
//!
//! Port origin: `Telegram/SourceFiles/info/profile/info_profile_members.cpp`,
//! `Telegram/SourceFiles/info/members/info_members_widget.cpp`,
//! and `Telegram/SourceFiles/info/common_groups/info_common_groups_widget.cpp`.

use egui::{Align2, CornerRadius, CursorIcon, FontId, Pos2, Rect, Sense, Ui, Vec2};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold, font_id_from_style};
use tg_ui::icons::paint_style_icon;
use tg_ui::stand_in::{Colors, InputFieldStyle};
use tg_ui::tg_style::{Icon, st};
use tg_ui::theme::ColorsResolver;
use tg_ui::userpic::EmptyUserpic;
use tg_ui::widgets::input_field::InputField;

use crate::info::backend::{CommonGroupEntry, MemberEntry, MemberRole};
use crate::info::strings;
use tg_app_core::stand_in::types::PeerId;

/// Events emitted by [`MembersSection::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MembersAction {
    /// Clicked a member row to view their profile.
    OpenMemberProfile(PeerId),
    /// Clicked the `+` button to add members.
    AddMembers,
    /// Clicked the edit-admin button for a member.
    EditAdminRights(PeerId),
    /// Clicked the edit custom tag / rank button for a member (`EditTagControl`).
    EditMemberTag(PeerId),
    /// Clicked the remove button for a member.
    RemoveMember(PeerId),
}

/// Member count from which tdesktop shows the members search field.
const SEARCH_MEMBERS_AFTER_COUNT: u32 = 20;
/// Name / status left edge (`st::infoMembersList.item.namePosition.x`).
const MEMBER_TEXT_LEFT: f32 = 79.0;

/// Renders a group's members list with inline search, admin badges, and quick actions.
pub struct MembersSection;

impl MembersSection {
    /// Renders the members header, optional inline search input, and member rows.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        ui: &mut Ui,
        id_salt: &str,
        total_count: u32,
        members: &[MemberEntry],
        search_query: &mut String,
        show_header: bool,
        can_add_members: bool,
        can_manage_admins: bool,
        colors: &dyn Colors,
    ) -> Option<MembersAction> {
        Self::show_with_ranks(
            ui,
            id_salt,
            total_count,
            members,
            search_query,
            show_header,
            can_add_members,
            can_manage_admins,
            colors,
            |_| None,
        )
    }

    /// Renders the members header, optional inline search input, and member rows with custom
    /// member rank/tag badges looked up via `rank_for` (`messages.editChatParticipantRank`).
    #[allow(clippy::too_many_arguments)]
    pub fn show_with_ranks(
        ui: &mut Ui,
        id_salt: &str,
        total_count: u32,
        members: &[MemberEntry],
        search_query: &mut String,
        show_header: bool,
        can_add_members: bool,
        can_manage_admins: bool,
        colors: &dyn Colors,
        rank_for: impl Fn(PeerId) -> Option<String>,
    ) -> Option<MembersAction> {
        let mut action = None;
        let width = ui.available_width().max(200.0);

        if show_header {
            // `Members::setupHeader`: `infoIconMembers` + uppercase count label + add button.
            let hdr_h = (st::info_members_header.0 as f32).max(40.0);
            let (hdr_rect, _) = ui.allocate_exact_size(Vec2::new(width, hdr_h), Sense::hover());
            let icon_rect = Rect::from_center_size(
                Pos2::new(hdr_rect.min.x + 30.0, hdr_rect.center().y),
                Vec2::splat(24.0),
            );
            paint_style_icon(
                ui.painter(),
                icon_rect,
                st::info_icon_members,
                &ColorsResolver(colors),
                Some(tg_ui::theme::resolve_color_ref(st::info_icon_fg, colors)),
            );
            let title = tr::lng_chat_status_members(i64::from(total_count)).to_uppercase();
            ui.painter().text(
                Pos2::new(hdr_rect.min.x + MEMBER_TEXT_LEFT, hdr_rect.center().y),
                Align2::LEFT_CENTER,
                title,
                FontId::new(
                    font_id_from_style(st::semibold_font).size,
                    family_semibold(),
                ),
                colors.color("windowBoldFg"),
            );
            if can_add_members {
                let add_rect = Rect::from_center_size(
                    Pos2::new(hdr_rect.max.x - 26.0, hdr_rect.center().y),
                    Vec2::splat(34.0),
                );
                if member_icon_btn(
                    ui,
                    (id_salt, "add_members"),
                    add_rect,
                    &st::info_members_add_member.icon,
                    &st::info_members_add_member.icon_over,
                    &tr::lng_profile_add_participant(),
                    colors,
                ) {
                    action = Some(MembersAction::AddMembers);
                }
            }
        }

        // Inline search bar (`kEnableSearchMembersAfterCount = 20`).
        let show_search = total_count >= SEARCH_MEMBERS_AFTER_COUNT || !search_query.is_empty();
        if show_search {
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                InputField::single_line(
                    &format!("{id_salt}_members_search"),
                    search_query,
                    strings::SEARCH_MEMBERS_PH,
                    InputFieldStyle::rounded_search(colors),
                )
                .width((width - 32.0).max(120.0))
                .show(ui);
            });
            ui.add_space(6.0);
        }

        if members.is_empty() {
            ui.add_space(12.0);
            let (r, _) = ui.allocate_exact_size(Vec2::new(width, 32.0), Sense::hover());
            ui.painter().text(
                r.center(),
                Align2::CENTER_CENTER,
                strings::NO_MATCHING_MEMBERS,
                font_id_from_style(st::normal_font),
                colors.color("windowSubTextFg"),
            );
            return action;
        }

        for member in members {
            let custom_rank = rank_for(member.peer_id);
            if let Some(ev) = member_row(
                ui,
                member,
                custom_rank.as_deref(),
                can_manage_admins,
                colors,
            ) {
                action = Some(ev);
            }
        }

        action
    }
}

fn member_row(
    ui: &mut Ui,
    member: &MemberEntry,
    custom_rank: Option<&str>,
    can_manage_admins: bool,
    colors: &dyn Colors,
) -> Option<MembersAction> {
    let mut action = None;
    let width = ui.available_width().max(200.0);
    let row_h = 54.0 + 2.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, row_h), Sense::click());
    let row_label = format!("Member: {}", member.name);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, row_label.clone()));
    let hovered = resp.hovered();
    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, colors.color("windowBgOver"));
    }

    // Avatar (`st::infoMembersList.item.photoPosition`, 42 px).
    let av_rect = Rect::from_min_size(
        Pos2::new(rect.min.x + 18.0, rect.min.y + 7.0),
        Vec2::splat(42.0),
    );
    EmptyUserpic::for_color_index(member.color_index, &member.name, colors)
        .paint_circle(ui.painter(), av_rect);

    // Online green dot indicator when online.
    if member.is_online {
        let dot_c = Pos2::new(av_rect.max.x - 5.0, av_rect.max.y - 5.0);
        ui.painter()
            .circle_filled(dot_c, 5.5, colors.color("windowBg"));
        ui.painter()
            .circle_filled(dot_c, 4.0, colors.color("dialogsOnlineBadgeFg"));
    }

    let text_left = rect.min.x + MEMBER_TEXT_LEFT;

    // Right side: admin/owner/custom-rank badge OR hover quick-manage buttons.
    let mut right_edge = rect.max.x - 16.0;
    let effective_badge = custom_rank
        .filter(|r| !r.is_empty())
        .or_else(|| member.role.badge_label());
    if can_manage_admins && hovered {
        let mut cursor_x = right_edge;
        if !matches!(member.role, MemberRole::Owner { .. }) {
            let rem_rect = Rect::from_center_size(
                Pos2::new(cursor_x - 14.0, rect.center().y),
                Vec2::splat(28.0),
            );
            if member_icon_btn(
                ui,
                ("member_rem", member.peer_id.0),
                rem_rect,
                &st::menu_icon_delete_attention,
                &st::menu_icon_delete_attention,
                strings::TOOLTIP_REMOVE_MEMBER,
                colors,
            ) {
                return Some(MembersAction::RemoveMember(member.peer_id));
            }
            cursor_x -= 30.0;

            let adm_rect = Rect::from_center_size(
                Pos2::new(cursor_x - 14.0, rect.center().y),
                Vec2::splat(28.0),
            );
            if member_icon_btn(
                ui,
                ("member_adm", member.peer_id.0),
                adm_rect,
                &st::menu_icon_admin,
                &st::menu_icon_admin,
                strings::TOOLTIP_EDIT_ADMIN_RIGHTS,
                colors,
            ) {
                return Some(MembersAction::EditAdminRights(member.peer_id));
            }
            cursor_x -= 30.0;
        }

        let tag_rect = Rect::from_center_size(
            Pos2::new(cursor_x - 14.0, rect.center().y),
            Vec2::splat(28.0),
        );
        if member_icon_btn(
            ui,
            ("member_tag", member.peer_id.0),
            tag_rect,
            &st::menu_icon_edit,
            &st::menu_icon_edit,
            &format!("Edit Member Tag: {}", member.name),
            colors,
        ) {
            return Some(MembersAction::EditMemberTag(member.peer_id));
        }
        right_edge = cursor_x - 34.0;
    } else if let Some(badge) = effective_badge {
        let badge_fg = match member.role {
            MemberRole::Owner { .. } | MemberRole::Admin { .. } => {
                colors.color("windowActiveTextFg")
            }
            MemberRole::Restricted { .. } => colors.color("attentionButtonFg"),
            MemberRole::Member => colors.color("windowSubTextFg"),
        };
        let badge_font = FontId::new(11.0, family_semibold());
        let galley = ui
            .painter()
            .layout_no_wrap(badge.to_owned(), badge_font.clone(), badge_fg);
        let pill_w = galley.size().x + 12.0;
        let pill = Rect::from_min_size(
            Pos2::new(right_edge - pill_w, rect.min.y + 12.0),
            Vec2::new(pill_w, 18.0),
        );
        ui.painter().rect_filled(
            pill,
            CornerRadius::same(4),
            colors.color("lightButtonBgOver"),
        );
        ui.painter().text(
            pill.center(),
            Align2::CENTER_CENTER,
            badge,
            badge_font,
            badge_fg,
        );
        right_edge = pill.min.x - 8.0;
    }

    let clip = Rect::from_min_max(
        Pos2::new(text_left, rect.min.y),
        Pos2::new(right_edge.max(text_left + 40.0), rect.max.y),
    );
    let p = ui.painter().with_clip_rect(clip);
    p.text(
        Pos2::new(text_left, rect.min.y + 11.0),
        Align2::LEFT_TOP,
        &member.name,
        font_id_from_style(st::semibold_font),
        colors.color("windowBoldFg"),
    );
    let st_fg = if member.is_online {
        colors.color("windowActiveTextFg")
    } else {
        colors.color("windowSubTextFg")
    };
    p.text(
        Pos2::new(text_left, rect.min.y + 31.0),
        Align2::LEFT_TOP,
        &member.status_text,
        FontId::new(font_id_from_style(st::normal_font).size, family_regular()),
        st_fg,
    );

    if can_manage_admins && resp.secondary_clicked() {
        action = Some(MembersAction::EditMemberTag(member.peer_id));
    } else if resp.clicked() {
        action = Some(MembersAction::OpenMemberProfile(member.peer_id));
    }
    action
}

fn member_icon_btn<H: std::hash::Hash + std::fmt::Debug>(
    ui: &mut Ui,
    id_salt: H,
    rect: Rect,
    icon: &Icon,
    icon_over: &Icon,
    tooltip: &str,
    colors: &dyn Colors,
) -> bool {
    let resp = ui.interact(rect, ui.make_persistent_id(id_salt), Sense::click());
    if !tooltip.is_empty() {
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tooltip));
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter().circle_filled(
            rect.center(),
            (rect.width().min(rect.height()) * 0.48).min(16.0),
            colors.color("windowBgRipple"),
        );
    }
    let active = if resp.hovered() && !icon_over.is_empty() {
        icon_over
    } else {
        icon
    };
    let ic_rect = Rect::from_center_size(rect.center(), Vec2::splat(18.0));
    paint_style_icon(
        ui.painter(),
        ic_rect,
        *active,
        &ColorsResolver(colors),
        None,
    );
    let resp = if !tooltip.is_empty() {
        resp.on_hover_text(tooltip)
    } else {
        resp
    };
    resp.clicked()
}

/// Renders the Groups in Common list (`Info::CommonGroups::Widget`).
pub struct CommonGroupsSection;

impl CommonGroupsSection {
    /// Renders shared groups for a user and returns the clicked group's [`PeerId`].
    pub fn show(ui: &mut Ui, groups: &[CommonGroupEntry], colors: &dyn Colors) -> Option<PeerId> {
        let mut clicked = None;
        let width = ui.available_width().max(200.0);

        if groups.is_empty() {
            let (r, _) = ui.allocate_exact_size(Vec2::new(width, 48.0), Sense::hover());
            ui.painter().text(
                r.center(),
                Align2::CENTER_CENTER,
                strings::NO_COMMON_GROUPS,
                font_id_from_style(st::normal_font),
                colors.color("windowSubTextFg"),
            );
            return None;
        }

        for group in groups {
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 54.0), Sense::click());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                ui.painter()
                    .rect_filled(rect, CornerRadius::ZERO, colors.color("windowBgOver"));
            }
            let av_rect = Rect::from_min_size(
                Pos2::new(rect.min.x + 18.0, rect.center().y - 19.0),
                Vec2::splat(38.0),
            );
            EmptyUserpic::for_color_index(group.color_index, &group.title, colors)
                .paint_circle(ui.painter(), av_rect);

            let text_x = av_rect.max.x + 12.0;
            ui.painter().text(
                Pos2::new(text_x, rect.min.y + 9.0),
                Align2::LEFT_TOP,
                &group.title,
                font_id_from_style(st::semibold_font),
                colors.color("windowBoldFg"),
            );
            let sub = tr::lng_chat_status_members(i64::from(group.members_count));
            ui.painter().text(
                Pos2::new(text_x, rect.min.y + 28.0),
                Align2::LEFT_TOP,
                sub,
                FontId::new(12.0, family_regular()),
                colors.color("windowSubTextFg"),
            );
            if resp.clicked() {
                clicked = Some(group.peer_id);
            }
        }
        clicked
    }
}
