//! Profile cover header: centered avatar, peer display name, verification/premium badges,
//! status, and the row of rounded action buttons (Message / Mute / Gift / Manage / Leave).
//!
//! Port origin: `Telegram/SourceFiles/info/profile/info_profile_top_bar.cpp`
//! (`TopBar::setupActions`, `info_profile_top_bar.style`),
//! `Telegram/SourceFiles/info/profile/info_profile_top_bar_action_button.cpp`,
//! and `Telegram/SourceFiles/info/profile/info_profile_badge.cpp`.

use egui::{Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Sense, Ui, Vec2};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold, font_id_from_style};
use tg_ui::icons::paint_style_icon;
use tg_ui::stand_in::Colors;
use tg_ui::style::px;
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
    /// Requested deleting own profile photo (`photos.deletePhotos`).
    DeleteOwnProfilePhoto,
    /// Requested suggesting or setting a personal photo for a contact (`photos.uploadContactProfilePhoto`).
    SuggestContactPhoto,
    /// Requested viewing sticker/mask sets attached to the profile photo (`messages.getAttachedStickers`).
    ViewAttachedStickers,
    /// Clicked one of the rounded action buttons under the status line.
    Button(CoverButton),
}

/// Rounded action buttons under the profile status (`TopBar::setupActions`, at most three).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverButton {
    /// `lng_profile_action_short_message`.
    Message,
    /// `lng_profile_action_short_mute` / `_unmute`.
    Mute,
    /// `lng_profile_action_short_call` (users, outside the side column).
    Call,
    /// `lng_profile_action_short_manage`.
    Manage,
    /// `lng_profile_action_short_video_chat` (groups) / `_live_stream` (channels).
    VideoChat {
        /// `peer->isBroadcast()`: the button reads "Live stream".
        live_stream: bool,
    },
    /// `lng_profile_action_short_gift`.
    Gift,
    /// `lng_profile_action_short_leave`.
    Leave,
}

impl CoverButton {
    fn label(self, muted: bool) -> String {
        match self {
            Self::Message => tr::lng_profile_action_short_message(),
            Self::Mute if muted => tr::lng_profile_action_short_unmute(),
            Self::Mute => tr::lng_profile_action_short_mute(),
            Self::Call => tr::lng_profile_action_short_call(),
            Self::Manage => tr::lng_profile_action_short_manage(),
            Self::VideoChat { live_stream: true } => tr::lng_profile_action_short_live_stream(),
            Self::VideoChat { live_stream: false } => tr::lng_profile_action_short_video_chat(),
            Self::Gift => tr::lng_profile_action_short_gift(),
            Self::Leave => tr::lng_profile_action_short_leave(),
        }
    }

    fn icon(self, muted: bool) -> tg_ui::tg_style::Icon {
        match self {
            Self::Message => st::info_profile_top_bar_action_message,
            Self::Mute if muted => st::info_profile_top_bar_action_unmute,
            Self::Mute => st::info_profile_top_bar_action_mute,
            Self::Call => st::info_profile_top_bar_action_call,
            Self::Manage => st::info_profile_top_bar_action_manage,
            Self::VideoChat { .. } => st::info_profile_top_bar_action_live_stream,
            Self::Gift => st::info_profile_top_bar_action_gift,
            Self::Leave => st::info_profile_top_bar_action_leave,
        }
    }
}

/// Action buttons tdesktop shows for `peer` (`TopBar::setupActions`, max three), in its order:
/// Message, Mute, Call, Manage, Video chat / Live stream, Gift, Leave. `is_side` is `true` for
/// the third-column profile, where tdesktop never shows Call (`!isSide`). Discuss is skipped
/// (Rustgram has no linked-chat peek data for it yet).
#[must_use]
pub fn cover_buttons(peer: &PeerProfile, is_side: bool) -> Vec<CoverButton> {
    let mut out = Vec::new();
    let is_self = peer.kind == InfoPeerKind::SavedMessages;
    if matches!(
        peer.kind,
        InfoPeerKind::User | InfoPeerKind::Bot | InfoPeerKind::SavedMessages
    ) {
        out.push(CoverButton::Message);
    }
    if !is_self {
        out.push(CoverButton::Mute);
    }
    // Bots and self have `CallsStatus::Disabled` (`user->sharedMediaInfo()` / bot users).
    if !is_side && peer.kind == InfoPeerKind::User {
        out.push(CoverButton::Call);
    }
    if (peer.kind.is_group() || peer.kind.is_channel()) && peer.can_edit {
        out.push(CoverButton::Manage);
    }
    // `peer->groupCall() || (!peer->isUser() && peer->canManageGroupCall())`.
    let is_user = matches!(
        peer.kind,
        InfoPeerKind::User | InfoPeerKind::Bot | InfoPeerKind::SavedMessages
    );
    if peer.group_call.active || (!is_user && peer.group_call.can_manage) {
        out.push(CoverButton::VideoChat {
            live_stream: peer.kind == InfoPeerKind::Channel,
        });
    }
    if peer.kind == InfoPeerKind::User {
        out.push(CoverButton::Gift);
    }
    if peer.kind.is_group() || peer.kind.is_channel() {
        out.push(CoverButton::Leave);
    }
    out.truncate(3);
    out
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
        Self::show_with_extras_and_photo_ref(ui, peer, colors, birthday_effect, balance, false)
    }

    /// Draws the profile cover with optional birthday confetti burst, `info_profile_balance_pill`,
    /// and whether a real server profile-photo reference is currently loaded for photo actions.
    pub fn show_with_extras_and_photo_ref(
        ui: &mut Ui,
        peer: &PeerProfile,
        colors: &dyn Colors,
        birthday_effect: Option<&mut ProfileBirthdayEffect>,
        balance: Option<&ProfileBalanceInfo>,
        has_active_photo_ref: bool,
    ) -> Option<CoverAction> {
        Self::show_with_buttons(
            ui,
            peer,
            colors,
            birthday_effect,
            balance,
            has_active_photo_ref,
            &[],
        )
    }

    /// Draws the 7.4 profile top bar: centered `infoProfileTopBarPhotoSize` avatar, 16 px
    /// semibold name, status, and `buttons` as rounded `TopBarActionButton`s.
    #[allow(clippy::too_many_lines)]
    pub fn show_with_buttons(
        ui: &mut Ui,
        peer: &PeerProfile,
        colors: &dyn Colors,
        birthday_effect: Option<&mut ProfileBirthdayEffect>,
        balance: Option<&ProfileBalanceInfo>,
        has_active_photo_ref: bool,
        buttons: &[CoverButton],
    ) -> Option<CoverAction> {
        let mut action = None;
        let width = ui.available_width().max(200.0);
        let buttons_h = px(st::info_profile_top_bar_action_buttons_height);
        let height = px(st::info_profile_top_bar_height_max)
            - if buttons.is_empty() { buttons_h } else { 0.0 };
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());

        // `TopBar::paintEdges`: without a colour profile the whole top bar (including the strip
        // under the floating Info top bar) is `boxDividerBg`.
        let bg_top = rect
            .top()
            .min(ui.clip_rect().top().max(rect.top() - height));
        ui.painter().rect_filled(
            Rect::from_min_max(Pos2::new(rect.left(), bg_top), rect.max),
            CornerRadius::ZERO,
            colors.color("boxDividerBg"),
        );

        // Avatar (`infoProfileTopBarPhotoSize` at `infoProfileTopBarPhotoTop`, centered).
        let av_size = px(st::info_profile_top_bar_photo_size);
        let av_rect = Rect::from_min_size(
            Pos2::new(
                rect.center().x - av_size * 0.5,
                rect.min.y + px(st::info_profile_top_bar_photo_top),
            ),
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
        av_resp.context_menu(|ui| {
            let unavailable_tip = "Profile photo metadata is not loaded yet.";
            if peer.kind == InfoPeerKind::SavedMessages {
                let resp = ui.add_enabled(
                    has_active_photo_ref,
                    egui::Button::new("Delete Profile Photo"),
                );
                if resp.on_disabled_hover_text(unavailable_tip).clicked() {
                    action = Some(CoverAction::DeleteOwnProfilePhoto);
                    ui.close();
                }
            } else {
                if peer.kind == InfoPeerKind::User
                    && ui.button(tr::lng_profile_suggest_photo()).clicked()
                {
                    action = Some(CoverAction::SuggestContactPhoto);
                    ui.close();
                }
                let resp = ui.add_enabled(
                    has_active_photo_ref,
                    egui::Button::new("Report Profile Photo"),
                );
                if resp.on_disabled_hover_text(unavailable_tip).clicked() {
                    action = Some(CoverAction::ReportProfilePhoto);
                    ui.close();
                }
            }
            let resp = ui.add_enabled(
                has_active_photo_ref,
                egui::Button::new(tr::lng_stickers_attached_sets()),
            );
            if resp.on_disabled_hover_text(unavailable_tip).clicked() {
                action = Some(CoverAction::ViewAttachedStickers);
                ui.close();
            }
        });

        // Name (`infoProfileCover.name`, 16 px semibold) centered at `infoProfileTopBarTitleTop`.
        let title_font = FontId::new(
            px(st::info_profile_cover.name.style.font.size),
            family_semibold(),
        );
        let bold_fg = colors.color("windowBoldFg");
        let badge_space = (if peer.verified { 20.0 } else { 0.0 })
            + (if peer.premium { 20.0 } else { 0.0 })
            + (if peer.kind == InfoPeerKind::Bot {
                36.0
            } else {
                0.0
            });
        let side = px(st::info_profile_top_bar_action_buttons_padding.left);
        let max_title_w = (width - side * 2.0 - badge_space).max(60.0);
        let name_galley =
            ui.painter()
                .layout_no_wrap(peer.title.clone(), title_font.clone(), bold_fg);
        let title_w = name_galley.size().x.min(max_title_w);
        let name_left = rect.center().x - (title_w + badge_space) * 0.5;
        let name_y = rect.min.y + px(st::info_profile_top_bar_title_top);
        let title_clip = Rect::from_min_max(
            Pos2::new(name_left, name_y - 2.0),
            Pos2::new(name_left + title_w, name_y + name_galley.size().y + 2.0),
        );
        ui.painter().with_clip_rect(title_clip).galley(
            Pos2::new(name_left, name_y),
            name_galley.clone(),
            bold_fg,
        );

        let mut badge_x = name_left + title_w + 2.0;
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
            let pill = Rect::from_min_size(
                Pos2::new(badge_x + 2.0, badge_cy - 8.5),
                Vec2::new(32.0, 17.0),
            );
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

        // Status (`infoProfileCover.status`) centered at `infoProfileTopBarStatusTop`.
        let status_fg = if peer.is_online {
            colors.color("windowActiveTextFg")
        } else {
            colors.color("windowSubTextFg")
        };
        let status_text = if peer.kind == InfoPeerKind::SavedMessages {
            String::new()
        } else {
            peer.status.clone()
        };
        ui.painter().text(
            Pos2::new(
                rect.center().x,
                rect.min.y + px(st::info_profile_top_bar_status_top),
            ),
            Align2::CENTER_TOP,
            status_text,
            FontId::new(font_id_from_style(st::normal_font).size, family_regular()),
            status_fg,
        );

        // Action buttons (`infoProfileTopBarActionButtonsPadding`, `...ButtonsSpace`).
        if !buttons.is_empty() {
            let pad = st::info_profile_top_bar_action_buttons_padding;
            let size = px(st::info_profile_top_bar_action_button_size);
            let space = px(st::info_profile_top_bar_action_buttons_space);
            let left = rect.min.x + px(pad.left);
            let avail = (width - px(pad.left) - px(pad.right)).max(60.0);
            let count = buttons.len() as f32;
            let button_w = ((avail - space * (count - 1.0)) / count).max(40.0);
            let top = rect.max.y - px(pad.bottom) - size;
            for (idx, button) in buttons.iter().enumerate() {
                let r = Rect::from_min_size(
                    Pos2::new(left + idx as f32 * (button_w + space), top),
                    Vec2::new(button_w, size),
                );
                if paint_action_button(ui, r, *button, peer.muted, idx, colors) {
                    action = Some(CoverAction::Button(*button));
                }
            }
        }

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

        // `Info::Profile::BalancePill` (`info_profile_balance_pill.cpp`) below the buttons.
        if let Some(bal) = balance
            && (bal.ton_nanos > 0 || bal.stars_balance > 0)
        {
            ui.horizontal(|ui| {
                ui.add_space(px(st::info_profile_top_bar_action_buttons_padding.left));
                if let Some(click) = ProfileBalancePill::show(ui, bal, colors) {
                    action = Some(CoverAction::BalancePillClicked(click));
                }
            });
            ui.add_space(4.0);
        }

        action
    }
}

/// Paints one `TopBarActionButton` (`boxRadius` card, 23 px icon at `IconTop`, 11 px label at
/// `TextTop`): Day uses `boxBg` at 84% with `infoProfileTopBarActionButtonShadow`, Night has no
/// shadow (`TopBar::mapActionStyle`).
fn paint_action_button(
    ui: &mut Ui,
    rect: Rect,
    button: CoverButton,
    muted: bool,
    idx: usize,
    colors: &dyn Colors,
) -> bool {
    let label = button.label(muted);
    let resp = ui.interact(
        rect,
        ui.make_persistent_id(("info_cover_action", idx)),
        Sense::click(),
    );
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label.clone()));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let radius = px(st::box_radius);
    let window_bg = colors.color("windowBg");
    let night =
        u32::from(window_bg.r()) + u32::from(window_bg.g()) + u32::from(window_bg.b()) < 384;
    if !night {
        ui.painter().rect_filled(
            rect.expand(1.0),
            CornerRadius::same((radius + 1.0) as u8),
            Color32::from_black_alpha(18),
        );
        ui.painter().rect_filled(
            rect.expand(0.5),
            CornerRadius::same((radius + 0.5) as u8),
            Color32::from_black_alpha(22),
        );
    }
    let box_bg = colors.color("boxBg");
    let bg = if night {
        colors.color("windowBgOver")
    } else {
        Color32::from_rgba_unmultiplied(box_bg.r(), box_bg.g(), box_bg.b(), 214)
    };
    ui.painter()
        .rect_filled(rect, CornerRadius::same(radius as u8), bg);
    if resp.hovered() {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(radius as u8),
            colors.color("windowBgRipple").gamma_multiply(0.5),
        );
    }
    let fg = colors.color("windowBoldFg");
    let icon_size = px(st::info_profile_top_bar_action_button_icon_size);
    let icon_rect = Rect::from_min_size(
        Pos2::new(
            rect.center().x - icon_size * 0.5,
            rect.min.y + px(st::info_profile_top_bar_action_button_icon_top),
        ),
        Vec2::splat(icon_size),
    );
    paint_style_icon(
        ui.painter(),
        icon_rect,
        button.icon(muted),
        &ColorsResolver(colors),
        Some(fg),
    );
    let label_font = FontId::new(
        px(st::info_profile_top_bar_action_button_font.size),
        family_regular(),
    );
    let pad = px(st::info_profile_top_bar_action_button_text_padding);
    let clip = Rect::from_min_max(
        Pos2::new(rect.min.x + pad, rect.min.y),
        Pos2::new(rect.max.x - pad, rect.max.y),
    );
    ui.painter().with_clip_rect(clip).text(
        Pos2::new(
            rect.center().x,
            rect.min.y + px(st::info_profile_top_bar_action_button_text_top),
        ),
        Align2::CENTER_TOP,
        label,
        label_font,
        fg,
    );
    resp.clicked()
}
