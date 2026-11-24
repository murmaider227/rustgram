//! Group / Channel invite preview modal box (`GroupInviteBox` / `JoinGroupByHash`).
//!
//! Port origin: `Telegram/SourceFiles/api/api_chat_invite.cpp`
//! and `Telegram/SourceFiles/boxes/peers/add_participants_box.cpp`.

use egui::{Align2, Pos2, Rect, Sense, Ui, Vec2};
use tg_lang::tr;
use tg_ui::fonts::font_id_from_style;
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{Colors, FlatLabelStyle};
use tg_ui::tg_style::st;
use tg_ui::userpic::EmptyUserpic;
use tg_ui::widgets::labels::FlatLabel;

/// Preview data returned by `messages.checkChatInvite` for a joinable chat or channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinInviteBoxState {
    /// Invite hash passed to `messages.importChatInvite` on confirmation.
    pub hash: String,
    /// Group or channel title.
    pub title: String,
    /// Optional chat description (`about`).
    pub about: Option<String>,
    /// Total participant / subscriber count.
    pub participants_count: i32,
    /// Whether this invite targets a channel (broadcast or supergroup).
    pub is_channel: bool,
    /// Whether this invite targets a broadcast channel (subscribers) vs group (members).
    pub is_broadcast: bool,
    /// Whether joining requires admin approval (`request_needed`).
    pub request_needed: bool,
    /// Whether the chat has a verified badge.
    pub verified: bool,
    /// Palette avatar color index.
    pub color_index: u8,
    /// Preview names of sample participants returned by the server.
    pub participant_names: Vec<String>,
}

impl JoinInviteBoxState {
    /// Builds the `tg-ui` [`BoxModal`] descriptor with Join / Request to Join + Cancel buttons.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        let heading = if self.is_broadcast {
            "Channel Invite"
        } else {
            "Group Invite"
        };
        let confirm_label = if self.request_needed {
            "Request to Join"
        } else if self.is_broadcast {
            "Join Channel"
        } else {
            "Join Group"
        };
        BoxModal::new(id, heading)
            .width(st::box_width.0 as f32 + 24.0)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::default_btn("join", confirm_label))
    }

    /// Renders the invite avatar, title, member/subscriber count, description, and participants.
    pub fn show_body(&self, ui: &mut Ui, colors: &dyn Colors) {
        let avail_w = ui.available_width().max(220.0);

        // Centered avatar circle.
        let avatar_size = 72.0;
        let (av_row, _) =
            ui.allocate_exact_size(Vec2::new(avail_w, avatar_size + 8.0), Sense::hover());
        let av_rect = Rect::from_center_size(av_row.center(), Vec2::splat(avatar_size));
        EmptyUserpic::for_color_index(self.color_index, &self.title, colors)
            .paint_circle(ui.painter(), av_rect);

        ui.add_space(6.0);

        // Title + optional verified indicator.
        let (title_rect, _) = ui.allocate_exact_size(Vec2::new(avail_w, 24.0), Sense::hover());
        let title_fg = colors.color("windowBoldFg");
        let title_galley = ui.painter().layout_no_wrap(
            self.title.clone(),
            font_id_from_style(st::box_title_font),
            title_fg,
        );
        let badge_w = if self.verified { 18.0 } else { 0.0 };
        let total_w = title_galley.size().x + badge_w;
        let left_x = title_rect.center().x - total_w * 0.5;
        let text_w = title_galley.size().x;
        ui.painter().galley(
            Pos2::new(left_x, title_rect.center().y - title_galley.size().y * 0.5),
            title_galley,
            title_fg,
        );
        if self.verified {
            tg_app_core::ui_icons::paint_select_check(
                ui.painter(),
                Pos2::new(left_x + text_w + 10.0, title_rect.center().y),
                14.0,
                colors.color("windowActiveTextFg"),
            );
        }

        // Subtitle (member/subscriber count).
        let unit = if self.is_broadcast {
            if self.participants_count == 1 {
                "subscriber"
            } else {
                "subscribers"
            }
        } else if self.participants_count == 1 {
            "member"
        } else {
            "members"
        };
        let subtitle = format!("{} {unit}", self.participants_count.max(0));
        let (sub_rect, _) = ui.allocate_exact_size(Vec2::new(avail_w, 20.0), Sense::hover());
        ui.painter().text(
            sub_rect.center(),
            Align2::CENTER_CENTER,
            subtitle,
            font_id_from_style(st::normal_font),
            colors.color("windowSubTextFg"),
        );

        if let Some(ref about) = self.about
            && !about.trim().is_empty()
        {
            ui.add_space(10.0);
            FlatLabel::plain(about, FlatLabelStyle::box_label(colors))
                .id_salt("join_invite_about")
                .show(ui);
        }

        if !self.participant_names.is_empty() {
            ui.add_space(10.0);
            let names = self.participant_names.join(", ");
            let (p_rect, _) = ui.allocate_exact_size(Vec2::new(avail_w, 20.0), Sense::hover());
            ui.painter().text(
                Pos2::new(p_rect.min.x, p_rect.center().y),
                Align2::LEFT_CENTER,
                format!("Participants: {names}"),
                font_id_from_style(st::normal_font),
                colors.color("windowSubTextFg"),
            );
        }

        if self.request_needed {
            ui.add_space(10.0);
            FlatLabel::plain(
                "An administrator will need to approve your request to join.",
                FlatLabelStyle::box_label(colors),
            )
            .id_salt("join_invite_approval_note")
            .show(ui);
        }
    }
}
