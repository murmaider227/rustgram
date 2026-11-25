//! Profile info value rows (phone, usernames, bio, birthday, link), notifications toggle,
//! shared media summary counters, and contextual peer action buttons.
//!
//! Port origin: `Telegram/SourceFiles/info/profile/info_profile_actions.cpp`
//! and `Telegram/SourceFiles/info/profile/info_profile_values.cpp`.

use egui::{Align2, CornerRadius, CursorIcon, FontId, Pos2, Rect, Sense, Ui, Vec2};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, font_id_from_style};
use tg_ui::icons::paint_style_icon;
use tg_ui::stand_in::Colors;
use tg_ui::tg_style::{Icon, st};
use tg_ui::theme::{ColorsResolver, resolve_color_ref};
use tg_ui::widgets::labels::divider_bar;

use crate::info::backend::{InfoPeerKind, PeerProfile, SharedMediaTab};
use crate::info::strings;
use crate::info::values::{
    DEFAULT_FRAGMENT_PHONE_PREFIXES, format_auto_delete_ttl, format_birthday, format_username_link,
    is_collectible_phone,
};
use tg_app_core::stand_in::types::PeerId;

/// User-triggered actions emitted by [`ProfileActionsView::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileAction {
    /// Open or return to the peer's chat history.
    SendMessage(PeerId),
    /// Toggle mute notifications for this peer.
    ToggleNotifications(PeerId),
    /// Copy a value (phone, username, link) and show a toast.
    CopyValue {
        /// Display label (e.g. `"Username"`, `"Invite link"`).
        label: String,
        /// Copied text.
        value: String,
    },
    /// Open a shared media sub-section tab.
    OpenSharedMedia(SharedMediaTab),
    /// Open the full Members sub-section.
    OpenMembers,
    /// Open the Groups in Common sub-section (for users).
    OpenCommonGroups,
    /// Open the Similar Channels sub-section (for broadcast channels).
    OpenSimilarChannels,
    /// Open the Profile Stories / Albums sub-section (`info/stories/*`).
    OpenStories,
    /// Open the Peer Star Gifts sub-section (`info/peer_gifts/*`).
    OpenPeerGifts,
    /// Open the Saved Profile Music sub-section (`info/saved/info_saved_music`).
    OpenSavedMusic,
    /// Open the Profile QR Card modal (`PeerQrBox`).
    OpenQrCode,
    /// Open the Emoji Userpic Builder modal (`info/userpic/*`).
    OpenEmojiUserpic,
    /// Open the Bot Verify Peers modal (`VerifyPeersBox`).
    OpenVerifyPeers,
    /// Open the Edit Managed Bot Info modal (`EditBotInfoBox`).
    OpenEditBotInfo,
    /// Open the Edit Peer Info modal box.
    OpenEditPeer,
    /// Open the Edit Username modal box.
    OpenEditUsername,
    /// Open the Group Permissions modal box.
    OpenPermissions,
    /// Open the Administrators modal box.
    OpenAdministrators,
    /// Open the Add Members picker box.
    OpenAddMembers,
    /// Open the Add Contact box.
    OpenAddContact,
    /// Open the Share / Forward box for this peer.
    OpenSharePeer,
    /// Open the Linked Discussion group profile or picker.
    OpenLinkedDiscussion(Option<PeerId>),
    /// Open the Auto-Delete timer selector.
    OpenAutoDelete,
    /// Open a Bot Mini App preview toast/action.
    OpenBotMiniApp(PeerId),
    /// Open Channel/Supergroup Statistics or Bot Stars Revenue.
    OpenStatistics(PeerId),
    /// Open Channel/Supergroup Boosts screen.
    OpenBoosts(PeerId),
    /// Toggle blocking a user or bot.
    ToggleBlock(PeerId),
    /// Open the Report modal box.
    OpenReport,
    /// Open the Delete / Leave confirmation box.
    OpenDeleteOrLeave,
    /// Open Fragment `CollectibleInfoBox` (`fragment.getCollectibleInfo`) for a collectible
    /// username or anonymous `+888` phone number on `peer_id`.
    OpenCollectibleInfo {
        /// Owning peer identifier.
        peer_id: PeerId,
        /// Collectible entity (username without `@`, or anonymous phone number with `+`).
        entity: String,
    },
    /// Open the inline Suggest Date of Birth dialog (`users.suggestBirthday`).
    SuggestBirthday,
    /// Open the inline Report Profile Photo dialog (`account.reportProfilePhoto`).
    ReportProfilePhoto,
    /// Open the inline Contact Note editor (`contacts.updateContactNote`).
    EditContactNote,
    /// Open the inline Delete Contact confirmation (`contacts.deleteContacts`).
    DeleteContact,
    /// Open the inline Delete Own Profile Photo confirmation (`photos.deletePhotos`).
    DeleteOwnProfilePhoto,
    /// Open the inline Suggest / Set Contact Profile Photo dialog (`photos.uploadContactProfilePhoto`).
    SuggestContactPhoto,
    /// Open Attached Stickers modal for this peer's profile photo (`messages.getAttachedStickers`).
    ViewAttachedStickers,
}

/// Shared media counter row: tab, count, icon and plural label (`lng_profile_photos`, ...).
type MediaCountRow = (SharedMediaTab, u32, Icon, fn(i64) -> String);

/// Renders the details, notifications toggle, media counts, and actions for a [`PeerProfile`].
pub struct ProfileActionsView;

impl ProfileActionsView {
    /// Renders all info values and action rows for `peer`.
    pub fn show(
        ui: &mut Ui,
        peer: &PeerProfile,
        in_third_column: bool,
        colors: &dyn Colors,
    ) -> Option<ProfileAction> {
        Self::show_with_today(
            ui,
            peer,
            in_third_column,
            crate::info::birthday_effect::current_utc_ymd(),
            colors,
        )
    }

    /// Renders all info values and action rows for `peer` using an explicit `(year, month, day)`
    /// date for `"Birthday today"` highlighting.
    pub fn show_with_today(
        ui: &mut Ui,
        peer: &PeerProfile,
        in_third_column: bool,
        today_ymd: (u16, u8, u8),
        colors: &dyn Colors,
    ) -> Option<ProfileAction> {
        Self::show_with_today_and_photo_ref(ui, peer, in_third_column, today_ymd, false, colors)
    }

    /// Renders all info values and action rows for `peer`, enabling profile-photo delete/report
    /// actions only when `has_active_photo_ref` is `true` (a real server `ProfilePhotoRef` is loaded).
    pub fn show_with_today_and_photo_ref(
        ui: &mut Ui,
        peer: &PeerProfile,
        in_third_column: bool,
        today_ymd: (u16, u8, u8),
        has_active_photo_ref: bool,
        colors: &dyn Colors,
    ) -> Option<ProfileAction> {
        let mut action = None;

        // 1. Info Details block (`info_profile_values.cpp`)
        let hidden_phone = phone_is_hidden(peer);
        let has_details = peer.phone.is_some()
            || hidden_phone
            || !peer.usernames.is_empty()
            || !peer.bio.is_empty()
            || peer.birthday.is_some()
            || (peer.invite_link.is_some() && peer.is_public)
            || peer.linked_discussion_peer.is_some()
            || peer.auto_delete_ttl_seconds > 0;

        if has_details {
            ui.add_space(INFO_SECTION_SKIP);
            if hidden_phone {
                // `PhoneOrHiddenValue`: no phone, username or bio -> "Hidden" (not copyable).
                info_value_row(
                    ui,
                    &tr::lng_info_mobile_hidden(),
                    &tr::lng_info_mobile_label(),
                    false,
                    colors,
                );
            } else if let Some(phone) = &peer.phone
                && !phone.is_empty()
            {
                let collectible_phone =
                    is_collectible_phone(phone, DEFAULT_FRAGMENT_PHONE_PREFIXES);
                let caption = if collectible_phone {
                    format!("{} · Collectible", tr::lng_info_mobile_label())
                } else {
                    tr::lng_info_mobile_label()
                };
                if info_value_row(ui, phone, &caption, collectible_phone, colors) {
                    if collectible_phone {
                        action = Some(ProfileAction::OpenCollectibleInfo {
                            peer_id: peer.id,
                            entity: phone.clone(),
                        });
                    } else {
                        action = Some(ProfileAction::CopyValue {
                            label: strings::PHONE_NUMBER_LABEL.to_owned(),
                            value: phone.clone(),
                        });
                    }
                }
            }

            if !peer.bio.is_empty() {
                let caption = match peer.kind {
                    InfoPeerKind::User | InfoPeerKind::Bot | InfoPeerKind::SavedMessages => {
                        tr::lng_info_bio_label()
                    }
                    InfoPeerKind::Group | InfoPeerKind::Supergroup | InfoPeerKind::Channel => {
                        tr::lng_info_about_label()
                    }
                };
                info_multiline_row(ui, &peer.bio, &caption, colors);
            }

            if let Some(primary) = peer.primary_username() {
                let is_peer_link = peer.kind.is_channel() || peer.kind.is_group();
                let value = if is_peer_link {
                    format_username_link(primary)
                } else {
                    format!("@{primary}")
                };
                let extra_collectibles: Vec<&str> = peer
                    .usernames
                    .iter()
                    .skip(1)
                    .filter(|u| u.is_active)
                    .map(|u| u.username.as_str())
                    .collect();
                let caption = if extra_collectibles.is_empty() {
                    if is_peer_link {
                        tr::lng_info_link_label()
                    } else {
                        tr::lng_info_username_label()
                    }
                } else {
                    let extras = extra_collectibles
                        .iter()
                        .map(|u| format!("@{u}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!(
                        "{} · {} {extras}",
                        tr::lng_info_username_label(),
                        tr::lng_info_usernames_label()
                    )
                };
                let primary_is_collectible = peer
                    .usernames
                    .iter()
                    .find(|u| u.is_active && u.username == primary)
                    .is_some_and(|u| !u.is_editable);
                let (clicked, qr_clicked) = info_value_row_with_qr(
                    ui,
                    &value,
                    &caption,
                    peer.kind != InfoPeerKind::SavedMessages,
                    colors,
                );
                if qr_clicked {
                    action = Some(ProfileAction::OpenQrCode);
                } else if clicked {
                    if primary_is_collectible {
                        action = Some(ProfileAction::OpenCollectibleInfo {
                            peer_id: peer.id,
                            entity: primary.to_owned(),
                        });
                    } else {
                        action = Some(ProfileAction::CopyValue {
                            label: if is_peer_link {
                                tr::lng_info_link_label()
                            } else {
                                tr::lng_info_username_label()
                            },
                            value: format_username_link(primary),
                        });
                    }
                }
                // Clickable Fragment collectible username chips (`info_profile_actions.cpp:1653`).
                let extra_collectible_entries: Vec<&str> = peer
                    .usernames
                    .iter()
                    .skip(1)
                    .filter(|u| u.is_active && !u.is_editable)
                    .map(|u| u.username.as_str())
                    .collect();
                if !extra_collectible_entries.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        ui.add_space(60.0);
                        for handle in extra_collectible_entries {
                            let label = format!("@{handle}");
                            if ui.small_button(label).clicked() {
                                action = Some(ProfileAction::OpenCollectibleInfo {
                                    peer_id: peer.id,
                                    entity: handle.to_owned(),
                                });
                            }
                        }
                    });
                }
            } else if let Some(invite) = peer.invite_link.as_ref().filter(|_| peer.is_public) {
                let invite_lbl = tr::lng_create_group_invite_link();
                if info_value_row(ui, invite, &invite_lbl, true, colors) {
                    action = Some(ProfileAction::CopyValue {
                        label: invite_lbl,
                        value: invite.clone(),
                    });
                }
            }

            if let Some(bday) = &peer.birthday {
                let (today_y, today_m, today_d) = today_ymd;
                let is_today =
                    crate::info::birthday_effect::is_birthday_today(*bday, today_m, today_d);
                let text = format_birthday(bday, Some(today_y));
                let label = if is_today {
                    tr::lng_info_birthday_today_label()
                } else {
                    tr::lng_info_birthday_label()
                };
                info_value_row(ui, &text, &label, is_today, colors);
            }

            if let Some((linked_id, linked_title)) = &peer.linked_discussion_peer {
                let caption = if peer.kind.is_channel() {
                    strings::LINKED_DISCUSSION_GROUP.to_owned()
                } else {
                    tr::lng_manage_linked_channel()
                };
                if info_value_row(ui, linked_title, &caption, true, colors) {
                    action = Some(ProfileAction::OpenLinkedDiscussion(Some(*linked_id)));
                }
            }

            if peer.auto_delete_ttl_seconds > 0 {
                let ttl_text = format!(
                    "{} {}",
                    strings::AUTO_DELETE_AFTER_PREFIX,
                    format_auto_delete_ttl(peer.auto_delete_ttl_seconds)
                );
                if info_value_row(ui, &ttl_text, strings::AUTO_DELETE_CAPTION, false, colors)
                    && peer.can_edit
                {
                    action = Some(ProfileAction::OpenAutoDelete);
                }
            }
        }

        // 2. "ADD TO CONTACTS" main button (`DetailsFiller::makeAddAsContact`).
        if peer.kind == InfoPeerKind::User
            && !peer.is_contact
            && main_button(ui, &tr::lng_info_add_as_contact().to_uppercase(), colors)
        {
            action = Some(ProfileAction::OpenAddContact);
        }
        let _ = (in_third_column, has_active_photo_ref);

        action
    }

    /// Renders the shared media counters (`1 photo`, `1 file`, ...) with tdesktop's
    /// `infoIconMedia*` icons (`info_profile_shared_media_classic.cpp`), plus groups in common.
    /// Returns `true` when at least one row was drawn.
    pub fn show_media_counts(
        ui: &mut Ui,
        peer: &PeerProfile,
        colors: &dyn Colors,
        action: &mut Option<ProfileAction>,
    ) -> bool {
        let counts = &peer.media_counts;
        let rows: [MediaCountRow; 6] = [
            (
                SharedMediaTab::PhotosVideos,
                counts.photos_videos,
                st::info_icon_media_photo,
                tr::lng_profile_photos,
            ),
            (
                SharedMediaTab::Files,
                counts.files,
                st::info_icon_media_file,
                tr::lng_profile_files,
            ),
            (
                SharedMediaTab::Music,
                counts.music,
                st::info_icon_media_audio,
                tr::lng_profile_songs,
            ),
            (
                SharedMediaTab::Links,
                counts.links,
                st::info_icon_media_link,
                tr::lng_profile_shared_links,
            ),
            (
                SharedMediaTab::Voice,
                counts.voice,
                st::info_icon_media_voice,
                tr::lng_profile_audios,
            ),
            (
                SharedMediaTab::Gifs,
                counts.gifs,
                st::info_icon_media_gif,
                tr::lng_profile_gifs,
            ),
        ];
        let mut any = false;
        for (tab, count, icon, label) in rows {
            if count == 0 {
                continue;
            }
            any = true;
            if action_row(
                ui,
                &icon,
                &label(i64::from(count)),
                None,
                false,
                false,
                colors,
            ) {
                *action = Some(ProfileAction::OpenSharedMedia(tab));
            }
        }
        if matches!(peer.kind, InfoPeerKind::User | InfoPeerKind::Bot) && counts.common_groups > 0 {
            any = true;
            let label = tr::lng_profile_common_groups(i64::from(counts.common_groups));
            if action_row(
                ui,
                &st::info_icon_media_group,
                &label,
                None,
                false,
                false,
                colors,
            ) {
                *action = Some(ProfileAction::OpenCommonGroups);
            }
        }
        any
    }

    /// Renders secondary peer actions (Share, Block, Report, Leave/Delete) below inline tabs.
    pub fn show_footer_actions(
        ui: &mut Ui,
        peer: &PeerProfile,
        colors: &dyn Colors,
    ) -> Option<ProfileAction> {
        Self::show_footer_actions_with_photo_ref(ui, peer, false, colors)
    }

    /// Renders secondary peer actions below inline tabs, enabling `"Report Profile Photo"` only
    /// when `has_active_photo_ref` is `true` (a real server `ProfilePhotoRef` is loaded).
    pub fn show_footer_actions_with_photo_ref(
        ui: &mut Ui,
        peer: &PeerProfile,
        has_active_photo_ref: bool,
        colors: &dyn Colors,
    ) -> Option<ProfileAction> {
        let mut action = None;
        if peer.kind == InfoPeerKind::SavedMessages {
            return None;
        }

        let _ = has_active_photo_ref;
        match peer.kind {
            InfoPeerKind::User | InfoPeerKind::Bot => {
                ui.add_space(INFO_SECTION_SKIP);
                divider_bar(ui, 8.0, colors);
                ui.add_space(INFO_SECTION_SKIP);
                let bot = peer.kind == InfoPeerKind::Bot;
                let label = match (bot, peer.is_blocked) {
                    (false, false) => tr::lng_profile_block_user(),
                    (false, true) => tr::lng_profile_unblock_user(),
                    (true, false) => tr::lng_profile_block_bot(),
                    (true, true) => tr::lng_profile_restart_bot(),
                };
                let icon = match (bot, peer.is_blocked) {
                    (_, false) => &st::info_icon_block,
                    (false, true) => &st::menu_icon_unblock,
                    (true, true) => &st::menu_icon_restart_bot,
                };
                if action_row(ui, icon, &label, None, !peer.is_blocked, false, colors) {
                    action = Some(ProfileAction::ToggleBlock(peer.id));
                }
            }
            InfoPeerKind::Group
            | InfoPeerKind::Supergroup
            | InfoPeerKind::Channel
            | InfoPeerKind::SavedMessages => {}
        }

        action
    }
}

/// `st::infoProfileButton`: padding (79, 10, 8, 8) around a 20 px text line, the 24 px icon at
/// `iconLeft` = 22 drawn at its natural size.
const PROFILE_BUTTON_HEIGHT: f32 = 38.0;
const PROFILE_BUTTON_ICON_LEFT: f32 = 22.0;
const PROFILE_BUTTON_ICON_SIZE: f32 = 24.0;
const PROFILE_BUTTON_TEXT_LEFT: f32 = 79.0;

/// `PhoneOrHiddenValue` (`info_profile_values.cpp`): a user (or bot) with no visible phone,
/// username and bio shows the phone line as "Hidden" instead of dropping it.
fn phone_is_hidden(peer: &PeerProfile) -> bool {
    matches!(peer.kind, InfoPeerKind::User | InfoPeerKind::Bot)
        && peer.phone.as_deref().is_none_or(str::is_empty)
        && peer.primary_username().is_none()
        && peer.bio.is_empty()
}

/// `defaultVerticalListSkip`, around the info blocks and dividers of the profile.
pub(crate) const INFO_SECTION_SKIP: f32 = 6.0;

/// `st::infoProfileLabeledPadding` = (23, 9, 20, 7): the labeled info lines
/// (`DetailsFiller::addInfoLine`) have no icon in 7.4.6; the text starts at 23 px.
const LABELED_PAD_LEFT: f32 = 23.0;
const LABELED_PAD_TOP: f32 = 9.0;
const LABELED_PAD_RIGHT: f32 = 20.0;
const LABELED_PAD_BOTTOM: f32 = 7.0;
/// One `defaultFlatLabel` line (13 px Open Sans) and the gap above the grey label under it.
const LABELED_LINE_HEIGHT: f32 = 19.0;
const LABELED_LABEL_SKIP: f32 = 2.0;

fn info_value_row(
    ui: &mut Ui,
    value: &str,
    caption: &str,
    is_link: bool,
    colors: &dyn Colors,
) -> bool {
    let width = ui.available_width().max(180.0);
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(
            width,
            LABELED_PAD_TOP + 2.0 * LABELED_LINE_HEIGHT + LABELED_LABEL_SKIP + LABELED_PAD_BOTTOM,
        ),
        Sense::click(),
    );
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::accesskit::Role::Label,
            true,
            format!("{value}, {caption}"),
        )
    });
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, colors.color("windowBgOver"));
    }
    let text_x = rect.min.x + LABELED_PAD_LEFT;
    let val_fg = if is_link {
        colors.color("windowActiveTextFg")
    } else {
        colors.color("windowFg")
    };
    let clip = Rect::from_min_max(
        Pos2::new(text_x, rect.min.y),
        Pos2::new(rect.max.x - LABELED_PAD_RIGHT, rect.max.y),
    );
    let p = ui.painter().with_clip_rect(clip);
    p.text(
        Pos2::new(text_x, rect.min.y + LABELED_PAD_TOP),
        Align2::LEFT_TOP,
        value,
        font_id_from_style(st::normal_font),
        val_fg,
    );
    p.text(
        Pos2::new(
            text_x,
            rect.min.y + LABELED_PAD_TOP + LABELED_LINE_HEIGHT + LABELED_LABEL_SKIP,
        ),
        Align2::LEFT_TOP,
        caption,
        font_id_from_style(st::normal_font),
        colors.color("windowSubTextFg"),
    );
    resp.clicked()
}

/// `info_value_row` with tdesktop's `infoProfileLabeledButtonQr` at the right edge.
/// Returns `(row_clicked, qr_clicked)`.
fn info_value_row_with_qr(
    ui: &mut Ui,
    value: &str,
    caption: &str,
    show_qr: bool,
    colors: &dyn Colors,
) -> (bool, bool) {
    let top = ui.cursor().min.y;
    let clicked = info_value_row(ui, value, caption, true, colors);
    if !show_qr {
        return (clicked, false);
    }
    let right = ui.min_rect().max.x.max(ui.max_rect().max.x);
    let row_h =
        LABELED_PAD_TOP + 2.0 * LABELED_LINE_HEIGHT + LABELED_LABEL_SKIP + LABELED_PAD_BOTTOM;
    let qr_rect = Rect::from_center_size(
        Pos2::new(right - 30.0, top + row_h * 0.5),
        Vec2::splat(34.0),
    );
    let resp = ui.interact(
        qr_rect,
        ui.make_persistent_id(("info_username_qr", value)),
        Sense::click(),
    );
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::accesskit::Role::Button, true, "QR Code"));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .circle_filled(qr_rect.center(), 17.0, colors.color("windowBgOver"));
    }
    paint_style_icon(
        ui.painter(),
        qr_rect,
        st::info_profile_labeled_button_qr.icon,
        &ColorsResolver(colors),
        Some(colors.color("windowActiveTextFg")),
    );
    (clicked && !resp.clicked(), resp.clicked())
}

/// Uppercase semibold `lightButtonFg` button (`st::infoMainButton`, `AddMainButton`).
fn main_button(ui: &mut Ui, label: &str, colors: &dyn Colors) -> bool {
    let width = ui.available_width().max(180.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 40.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::accesskit::Role::Button, true, label));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, colors.color("windowBgOver"));
    }
    ui.painter().text(
        Pos2::new(rect.min.x + 23.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::new(
            font_id_from_style(st::semibold_font).size,
            tg_ui::fonts::family_semibold(),
        ),
        colors.color("lightButtonFg"),
    );
    resp.clicked()
}

fn info_multiline_row(ui: &mut Ui, value: &str, caption: &str, colors: &dyn Colors) {
    let width = ui.available_width().max(180.0);
    let text_w = (width - LABELED_PAD_LEFT - LABELED_PAD_RIGHT).max(100.0);
    let galley = ui.painter().layout(
        value.to_owned(),
        font_id_from_style(st::normal_font),
        colors.color("windowFg"),
        text_w,
    );
    let total_h = LABELED_PAD_TOP
        + galley.size().y.max(LABELED_LINE_HEIGHT)
        + LABELED_LABEL_SKIP
        + LABELED_LINE_HEIGHT
        + LABELED_PAD_BOTTOM;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, total_h), Sense::hover());
    let text_x = rect.min.x + LABELED_PAD_LEFT;
    ui.painter().galley(
        Pos2::new(text_x, rect.min.y + LABELED_PAD_TOP),
        galley.clone(),
        colors.color("windowFg"),
    );
    ui.painter().text(
        Pos2::new(
            text_x,
            rect.min.y
                + LABELED_PAD_TOP
                + galley.size().y.max(LABELED_LINE_HEIGHT)
                + LABELED_LABEL_SKIP,
        ),
        Align2::LEFT_TOP,
        caption,
        font_id_from_style(st::normal_font),
        colors.color("windowSubTextFg"),
    );
}

fn action_row(
    ui: &mut Ui,
    icon: &Icon,
    label: &str,
    right_badge: Option<&str>,
    attention: bool,
    accent: bool,
    colors: &dyn Colors,
) -> bool {
    action_row_enabled(
        ui,
        icon,
        label,
        right_badge,
        attention,
        accent,
        true,
        colors,
    )
}

#[allow(clippy::too_many_arguments)]
fn action_row_enabled(
    ui: &mut Ui,
    icon: &Icon,
    label: &str,
    right_badge: Option<&str>,
    attention: bool,
    accent: bool,
    enabled: bool,
    colors: &dyn Colors,
) -> bool {
    let width = ui.available_width().max(180.0);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, PROFILE_BUTTON_HEIGHT), sense);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::accesskit::Role::Button, enabled, label));
    if enabled && resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, colors.color("windowBgOver"));
    }
    let fg = if !enabled {
        colors.color("windowSubTextFg")
    } else if attention {
        colors.color("attentionButtonFg")
    } else if accent {
        colors.color("windowActiveTextFg")
    } else {
        colors.color("windowFg")
    };
    let icon_fg = if !enabled {
        colors.color("windowSubTextFg")
    } else if attention {
        colors.color("attentionButtonFg")
    } else if accent {
        colors.color("windowActiveTextFg")
    } else {
        resolve_color_ref(st::info_icon_fg, colors)
    };
    let icon_rect = Rect::from_min_size(
        Pos2::new(
            rect.min.x + PROFILE_BUTTON_ICON_LEFT,
            rect.center().y - PROFILE_BUTTON_ICON_SIZE * 0.5,
        ),
        Vec2::splat(PROFILE_BUTTON_ICON_SIZE),
    );
    paint_style_icon(
        ui.painter(),
        icon_rect,
        *icon,
        &ColorsResolver(colors),
        Some(icon_fg),
    );

    ui.painter().text(
        Pos2::new(rect.min.x + PROFILE_BUTTON_TEXT_LEFT, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font_id_from_style(st::info_profile_button.style.font),
        fg,
    );

    if let Some(badge) = right_badge {
        ui.painter().text(
            Pos2::new(rect.max.x - 18.0, rect.center().y),
            Align2::RIGHT_CENTER,
            badge,
            FontId::new(12.5, family_regular()),
            if !enabled {
                colors.color("windowSubTextFg")
            } else if attention {
                colors.color("attentionButtonFg")
            } else {
                colors.color("windowActiveTextFg")
            },
        );
    }

    if !enabled {
        resp.on_hover_text("Profile photo metadata is not loaded yet.");
        return false;
    }
    resp.clicked()
}
