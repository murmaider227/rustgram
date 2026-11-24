//! Profile cover header: avatar circle, peer display name, verification/premium badges, and status.
//!
//! Port origin: `Telegram/SourceFiles/info/profile/info_profile_cover.h`,
//! `Telegram/SourceFiles/info/profile/info_profile_cover.cpp`,
//! and `Telegram/SourceFiles/info/profile/info_profile_badge.cpp`.

use egui::{Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Sense, Ui, Vec2};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold, font_id_from_style};
use tg_ui::icons::paint_style_icon;
use tg_ui::stand_in::Colors;
use tg_ui::tg_style::st;
use tg_ui::theme::ColorsResolver;
use tg_ui::userpic::EmptyUserpic;

use crate::info::backend::{InfoPeerKind, PeerProfile, ProfileBalanceInfo};
use crate::info::balance_pill::{BalancePillClick, ProfileBalancePill};
use crate::info::birthday_effect::ProfileBirthdayEffect;
use crate::info::strings;

/// Events emitted by [`ProfileCover::show`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverAction {
    /// Clicked the avatar circle (opens photo viewer or photo crop editor).
    AvatarClicked,
    /// Clicked the TON or Stars `info_profile_balance_pill` (`info_profile_balance_pill.cpp`).
    BalancePillClicked(BalancePillClick),
    /// Requested reporting this peer's profile photo (`account.reportProfilePhoto`).
    ReportProfilePhoto,
}

/// Renders the profile header block (`Info::Profile::Cover`).
pub struct ProfileCover;

impl ProfileCover {
    /// Draws the profile cover using `st::info_profile_cover` metrics, real `st::info_peer_badge`
    /// verified/premium icons, and semibold display name + presence subtitle.
    pub fn show(ui: &mut Ui, peer: &PeerProfile, colors: &dyn Colors) -> Option<CoverAction> {
        Self::show_with_extras(ui, peer, colors, None, None)
    }

    /// Draws the profile cover and overlays an active [`ProfileBirthdayEffect`] confetti burst
    /// when the peer's birthday matches today (`info_profile_birthday_effect.cpp`).
    pub fn show_with_birthday_effect(
        ui: &mut Ui,
        peer: &PeerProfile,
        colors: &dyn Colors,
        birthday_effect: Option<&mut ProfileBirthdayEffect>,
    ) -> Option<CoverAction> {
        Self::show_with_extras(ui, peer, colors, birthday_effect, None)
    }

    /// Draws the profile cover with optional birthday confetti burst and `info_profile_balance_pill`.
    pub fn show_with_extras(
        ui: &mut Ui,
        peer: &PeerProfile,
        colors: &dyn Colors,
        birthday_effect: Option<&mut ProfileBirthdayEffect>,
        balance: Option<&ProfileBalanceInfo>,
    ) -> Option<CoverAction> {
        let mut action = None;
        let width = ui.available_width().max(200.0);
        let height = st::info_profile_cover.height.0 as f32;
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());

        // Avatar (`st::info_profile_cover.photo.photo_size`, 72px diameter).
        let av_size = st::info_profile_cover.photo.photo_size.0 as f32;
        let av_rect = Rect::from_min_size(
            Pos2::new(rect.min.x + 20.0, rect.center().y - av_size * 0.5),
            Vec2::splat(av_size),
        );
        let av_resp = ui.interact(
            av_rect,
            ui.make_persistent_id(("info_cover_avatar", peer.id.0)),
            Sense::click(),
        );
        if av_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if peer.kind == InfoPeerKind::SavedMessages {
            EmptyUserpic::paint_saved_messages(ui.painter(), av_rect, colors);
        } else {
            EmptyUserpic::for_color_index(peer.color_index, &peer.title, colors)
                .paint_circle(ui.painter(), av_rect);
        }
        if av_resp.hovered() && peer.can_edit {
            ui.painter().circle_filled(
                av_rect.center(),
                av_size * 0.5,
                Color32::from_rgba_unmultiplied(0, 0, 0, 72),
            );
            ui.painter().text(
                av_rect.center(),
                Align2::CENTER_CENTER,
                strings::COVER_EDIT_OVERLAY,
                FontId::new(12.0, family_semibold()),
                Color32::WHITE,
            );
        }
        if av_resp.clicked() {
            action = Some(CoverAction::AvatarClicked);
        }
        if peer.kind != InfoPeerKind::SavedMessages {
            av_resp.context_menu(|ui| {
                if ui.button("Report Profile Photo").clicked() {
                    action = Some(CoverAction::ReportProfilePhoto);
                    ui.close();
                }
            });
        }

        // Right text column: display name + badges + status line.
        let text_left = av_rect.max.x + 16.0;
        let text_right = rect.max.x - 16.0;
        let name_y = av_rect.min.y + 14.0;
        let title_font = FontId::new(17.0, family_semibold());
        let bold_fg = colors.color("windowBoldFg");
        let name_galley =
            ui.painter()
                .layout_no_wrap(peer.title.clone(), title_font.clone(), bold_fg);

        let badge_space = (if peer.verified { 22.0 } else { 0.0 })
            + (if peer.premium { 22.0 } else { 0.0 })
            + (if peer.kind == InfoPeerKind::Bot {
                38.0
            } else {
                0.0
            });
        let max_title_w = (text_right - text_left - badge_space).max(60.0);
        let title_clip = Rect::from_min_max(
            Pos2::new(text_left, name_y - 2.0),
            Pos2::new(text_left + max_title_w, name_y + 26.0),
        );
        ui.painter().with_clip_rect(title_clip).text(
            Pos2::new(text_left, name_y),
            Align2::LEFT_TOP,
            &peer.title,
            title_font,
            bold_fg,
        );

        let mut badge_x = text_left
            + name_galley.size().x.min(max_title_w)
            + st::info_peer_badge.position.x.0 as f32
            + 2.0;
        let badge_cy = name_y + name_galley.size().y * 0.5;
        let resolver = ColorsResolver(colors);

        if peer.verified {
            let icon_rect =
                Rect::from_center_size(Pos2::new(badge_x + 9.0, badge_cy), Vec2::splat(18.0));
            paint_style_icon(
                ui.painter(),
                icon_rect,
                st::info_peer_badge.verified,
                &resolver,
                None,
            );
            paint_style_icon(
                ui.painter(),
                icon_rect,
                st::info_peer_badge.verified_check,
                &resolver,
                None,
            );
            badge_x += 20.0;
        }
        if peer.premium {
            let icon_rect =
                Rect::from_center_size(Pos2::new(badge_x + 9.0, badge_cy), Vec2::splat(18.0));
            paint_style_icon(
                ui.painter(),
                icon_rect,
                st::info_peer_badge.premium,
                &resolver,
                None,
            );
            badge_x += 20.0;
        }
        if peer.kind == InfoPeerKind::Bot {
            let pill =
                Rect::from_min_size(Pos2::new(badge_x, badge_cy - 8.5), Vec2::new(32.0, 17.0));
            ui.painter()
                .rect_filled(pill, CornerRadius::same(4), colors.color("windowBgOver"));
            let bot_tag = tr::lng_status_bot().to_uppercase();
            ui.painter().text(
                pill.center(),
                Align2::CENTER_CENTER,
                if bot_tag.len() <= 5 {
                    bot_tag
                } else {
                    strings::COVER_BOT_BADGE.to_owned()
                },
                FontId::new(9.5, family_semibold()),
                colors.color("windowActiveTextFg"),
            );
        }

        // Presence / members / subscribers subtitle.
        let status_fg = if peer.is_online {
            colors.color("windowActiveTextFg")
        } else {
            colors.color("windowSubTextFg")
        };
        ui.painter().text(
            Pos2::new(text_left, name_y + 26.0),
            Align2::LEFT_TOP,
            &peer.status,
            FontId::new(font_id_from_style(st::normal_font).size, family_regular()),
            status_fg,
        );

        if let Some(effect) = birthday_effect
            && effect.is_active()
        {
            effect.paint(
                ui.painter(),
                rect,
                av_rect,
                colors.color("windowActiveTextFg"),
            );
            ui.ctx().request_repaint();
        }

        // `Info::Profile::BalancePill` (`info_profile_balance_pill.cpp`) positioned cleanly
        // beneath the cover header aligned with the profile text column.
        if let Some(bal) = balance
            && (bal.ton_nanos > 0 || bal.stars_balance > 0)
        {
            ui.horizontal(|ui| {
                ui.add_space(20.0);
                if let Some(click) = ProfileBalancePill::show(ui, bal, colors) {
                    action = Some(CoverAction::BalancePillClicked(click));
                }
            });
            ui.add_space(4.0);
        }

        action
    }
}
