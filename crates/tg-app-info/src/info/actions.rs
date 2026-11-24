//! Profile info value rows (phone, usernames, bio, birthday, link), notifications toggle,
//! shared media summary counters, and contextual peer action buttons.
//!
//! Port origin: `Telegram/SourceFiles/info/profile/info_profile_actions.cpp`
//! and `Telegram/SourceFiles/info/profile/info_profile_values.cpp`.

use egui::{Align2, CornerRadius, CursorIcon, FontId, Pos2, Rect, Sense, Ui, Vec2};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, font_id_from_style};
use tg_ui::icons::paint_style_icon;
use tg_ui::stand_in::{Colors, ToggleStyle};
use tg_ui::tg_style::{Icon, st};
use tg_ui::theme::{ColorsResolver, resolve_color_ref};
use tg_ui::widgets::checkbox::Toggle;
use tg_ui::widgets::labels::divider_bar;

use crate::info::backend::{InfoPeerKind, PeerProfile, SharedMediaTab};
use crate::info::strings;
use crate::info::values::{
    DEFAULT_FRAGMENT_PHONE_PREFIXES, format_auto_delete_ttl, format_birthday, format_slowmode,
    format_username_link, is_collectible_phone,
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
}

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
        let mut action = None;

        // 1. Info Details block (`info_profile_values.cpp`)
        let has_details = peer.phone.is_some()
            || !peer.usernames.is_empty()
            || !peer.bio.is_empty()
            || peer.birthday.is_some()
            || peer.invite_link.is_some()
            || peer.linked_discussion_peer.is_some()
            || peer.auto_delete_ttl_seconds > 0;

        if has_details {
            ui.add_space(4.0);
            let mut info_icon_shown = false;
            if let Some(phone) = &peer.phone
                && !phone.is_empty()
            {
                info_icon_shown = true;
                let collectible_phone =
                    is_collectible_phone(phone, DEFAULT_FRAGMENT_PHONE_PREFIXES);
                let caption = if collectible_phone {
                    format!("{} · Collectible", tr::lng_info_mobile_label())
                } else {
                    tr::lng_info_mobile_label()
                };
                if info_value_row(
                    ui,
                    phone,
                    &caption,
                    Some(&st::info_icon_information),
                    collectible_phone,
                    colors,
                ) {
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
                let leading = if !info_icon_shown {
                    info_icon_shown = true;
                    Some(&st::info_icon_information)
                } else {
                    None
                };
                info_multiline_row(ui, &peer.bio, &caption, leading, colors);
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
                let leading = if !info_icon_shown {
                    info_icon_shown = true;
                    Some(&st::info_icon_information)
                } else {
                    None
                };
                let primary_is_collectible = peer
                    .usernames
                    .iter()
                    .find(|u| u.is_active && u.username == primary)
                    .is_some_and(|u| !u.is_editable);
                if info_value_row(ui, &value, &caption, leading, true, colors) {
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
            } else if let Some(invite) = &peer.invite_link {
                let leading = if !info_icon_shown {
                    info_icon_shown = true;
                    Some(&st::info_icon_information)
                } else {
                    None
                };
                let invite_lbl = tr::lng_create_group_invite_link();
                if info_value_row(ui, invite, &invite_lbl, leading, true, colors) {
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
                let leading = if !info_icon_shown {
                    info_icon_shown = true;
                    Some(&st::info_icon_information)
                } else {
                    None
                };
                let label = if is_today {
                    tr::lng_info_birthday_today_label()
                } else {
                    tr::lng_info_birthday_label()
                };
                info_value_row(ui, &text, &label, leading, is_today, colors);
            }

            if let Some((linked_id, linked_title)) = &peer.linked_discussion_peer {
                let caption = if peer.kind.is_channel() {
                    strings::LINKED_DISCUSSION_GROUP.to_owned()
                } else {
                    tr::lng_manage_linked_channel()
                };
                let leading = if !info_icon_shown {
                    info_icon_shown = true;
                    Some(&st::info_icon_information)
                } else {
                    None
                };
                if info_value_row(ui, linked_title, &caption, leading, true, colors) {
                    action = Some(ProfileAction::OpenLinkedDiscussion(Some(*linked_id)));
                }
            }

            if peer.auto_delete_ttl_seconds > 0 {
                let ttl_text = format!(
                    "{} {}",
                    strings::AUTO_DELETE_AFTER_PREFIX,
                    format_auto_delete_ttl(peer.auto_delete_ttl_seconds)
                );
                let leading = if !info_icon_shown {
                    Some(&st::menu_icon_ttl)
                } else {
                    None
                };
                if info_value_row(
                    ui,
                    &ttl_text,
                    strings::AUTO_DELETE_CAPTION,
                    leading,
                    false,
                    colors,
                ) && peer.can_edit
                {
                    action = Some(ProfileAction::OpenAutoDelete);
                }
            }
        }

        // 2. Notifications toggle row (hidden for Saved Messages)
        if peer.kind != InfoPeerKind::SavedMessages {
            ui.add_space(4.0);
            let mut notifications_on = !peer.muted;
            if toggle_action_row(
                ui,
                &tr::lng_chart_title_mute(),
                &mut notifications_on,
                colors,
            ) {
                action = Some(ProfileAction::ToggleNotifications(peer.id));
            }
        }

        // Primary action button (Send Message / Open Mini App)
        if !in_third_column && peer.kind != InfoPeerKind::SavedMessages {
            let label = if peer.kind.is_channel() {
                strings::OPEN_CHANNEL.to_owned()
            } else {
                tr::lng_profile_send_message()
            };
            if action_row(ui, &st::menu_icon_send, &label, None, false, true, colors) {
                action = Some(ProfileAction::SendMessage(peer.id));
            }
        }
        if peer.kind == InfoPeerKind::Bot
            && peer.has_mini_app
            && action_row(
                ui,
                &st::menu_icon_star,
                &tr::lng_profile_open_app(),
                Some(strings::MINI_APP_BADGE),
                false,
                true,
                colors,
            )
        {
            action = Some(ProfileAction::OpenBotMiniApp(peer.id));
        }
        if peer.kind == InfoPeerKind::Bot
            && action_row(
                ui,
                &st::menu_icon_stats,
                &tr::lng_bot_earn_title(),
                None,
                false,
                false,
                colors,
            )
        {
            action = Some(ProfileAction::OpenStatistics(peer.id));
        }
        if peer.kind == InfoPeerKind::Bot
            && action_row(
                ui,
                &st::menu_icon_admin,
                "Verify Peers",
                None,
                false,
                false,
                colors,
            )
        {
            action = Some(ProfileAction::OpenVerifyPeers);
        }
        if peer.kind == InfoPeerKind::Bot
            && peer.can_edit
            && action_row(
                ui,
                &st::info_icon_edit,
                "Edit Bot Info",
                None,
                false,
                false,
                colors,
            )
        {
            action = Some(ProfileAction::OpenEditBotInfo);
        }

        ui.add_space(4.0);

        // Extended Profile tabs & utilities (WP-26): Stories, Peer Gifts, Saved Music, QR Code, Emoji Avatar
        if matches!(
            peer.kind,
            InfoPeerKind::User | InfoPeerKind::SavedMessages | InfoPeerKind::Channel
        ) && action_row(
            ui,
            &st::menu_icon_boosts,
            "Stories & Albums",
            None,
            false,
            false,
            colors,
        ) {
            action = Some(ProfileAction::OpenStories);
        }
        if matches!(
            peer.kind,
            InfoPeerKind::User | InfoPeerKind::SavedMessages | InfoPeerKind::Channel
        ) && action_row(ui, &st::menu_icon_star, "Gifts", None, false, false, colors)
        {
            action = Some(ProfileAction::OpenPeerGifts);
        }
        if matches!(peer.kind, InfoPeerKind::User | InfoPeerKind::SavedMessages)
            && action_row(
                ui,
                &st::info_icon_media_group,
                "Saved Music",
                None,
                false,
                false,
                colors,
            )
        {
            action = Some(ProfileAction::OpenSavedMusic);
        }
        if peer.kind != InfoPeerKind::SavedMessages
            && action_row(
                ui,
                &st::info_icon_share,
                "QR Code",
                None,
                false,
                false,
                colors,
            )
        {
            action = Some(ProfileAction::OpenQrCode);
        }
        if peer.can_edit
            && action_row(
                ui,
                &st::menu_icon_palette,
                "Emoji Avatar",
                None,
                false,
                false,
                colors,
            )
        {
            action = Some(ProfileAction::OpenEmojiUserpic);
        }
        if peer.kind == InfoPeerKind::User
            && action_row(
                ui,
                &st::menu_icon_star,
                &tr::lng_suggest_birthday(),
                None,
                false,
                false,
                colors,
            )
        {
            action = Some(ProfileAction::SuggestBirthday);
        }

        if matches!(peer.kind, InfoPeerKind::User | InfoPeerKind::Bot)
            && peer.media_counts.common_groups > 0
        {
            let label = tr::lng_profile_common_groups(i64::from(peer.media_counts.common_groups));
            if action_row(
                ui,
                &st::info_icon_media_group,
                &label,
                None,
                false,
                false,
                colors,
            ) {
                action = Some(ProfileAction::OpenCommonGroups);
            }
        }

        // Management summary rows on groups/channels (compact right above Members/Media tabs)
        if peer.kind.is_group() || peer.kind.is_channel() {
            ui.add_space(4.0);
            divider_bar(ui, 6.0, colors);
            ui.add_space(4.0);

            if peer.can_edit {
                let manage_label = if peer.kind.is_channel() {
                    tr::lng_manage_channel_title()
                } else {
                    tr::lng_manage_group_title()
                };
                if action_row(
                    ui,
                    &st::info_icon_edit,
                    &manage_label,
                    None,
                    false,
                    false,
                    colors,
                ) {
                    action = Some(ProfileAction::OpenEditPeer);
                }
            }
            if peer.can_manage_permissions && peer.kind.is_group() {
                let badge = if peer.slowmode_seconds > 0 {
                    format!(
                        "{} {}",
                        strings::SLOW_MODE_PREFIX,
                        format_slowmode(peer.slowmode_seconds)
                    )
                } else {
                    strings::EDIT_BADGE.to_owned()
                };
                if action_row(
                    ui,
                    &st::menu_icon_permissions,
                    &tr::lng_manage_peer_permissions(),
                    Some(&badge),
                    false,
                    false,
                    colors,
                ) {
                    action = Some(ProfileAction::OpenPermissions);
                }
            }
            if peer.can_manage_admins {
                let count_s = peer.admins_count.to_string();
                if action_row(
                    ui,
                    &st::menu_icon_admin,
                    &tr::lng_manage_peer_administrators(),
                    Some(&count_s),
                    false,
                    false,
                    colors,
                ) {
                    action = Some(ProfileAction::OpenAdministrators);
                }
            }
            if action_row(
                ui,
                &st::menu_icon_stats,
                &tr::lng_stats_title(),
                None,
                false,
                false,
                colors,
            ) {
                action = Some(ProfileAction::OpenStatistics(peer.id));
            }
            if action_row(
                ui,
                &st::menu_icon_boosts,
                &tr::lng_boosts_title(),
                None,
                false,
                false,
                colors,
            ) {
                action = Some(ProfileAction::OpenBoosts(peer.id));
            }
            if peer.kind.is_channel()
                && action_row(
                    ui,
                    &st::info_icon_media_group,
                    &tr::lng_similar_channels_title(),
                    None,
                    false,
                    false,
                    colors,
                )
            {
                action = Some(ProfileAction::OpenSimilarChannels);
            }
        }

        action
    }

    /// Renders secondary peer actions (Share, Block, Report, Leave/Delete) below inline tabs.
    pub fn show_footer_actions(
        ui: &mut Ui,
        peer: &PeerProfile,
        colors: &dyn Colors,
    ) -> Option<ProfileAction> {
        let mut action = None;
        if peer.kind == InfoPeerKind::SavedMessages {
            return None;
        }

        ui.add_space(4.0);
        divider_bar(ui, 8.0, colors);
        ui.add_space(4.0);

        match peer.kind {
            InfoPeerKind::User => {
                if !peer.is_contact
                    && action_row(
                        ui,
                        &st::info_icon_add_member,
                        &tr::lng_info_add_as_contact(),
                        None,
                        false,
                        false,
                        colors,
                    )
                {
                    action = Some(ProfileAction::OpenAddContact);
                }
                if action_row(
                    ui,
                    &st::info_icon_share,
                    &tr::lng_info_share_contact(),
                    None,
                    false,
                    false,
                    colors,
                ) {
                    action = Some(ProfileAction::OpenSharePeer);
                }
                if peer.can_edit
                    && action_row(
                        ui,
                        &st::info_icon_edit,
                        &tr::lng_chatbots_edit_username(),
                        None,
                        false,
                        false,
                        colors,
                    )
                {
                    action = Some(ProfileAction::OpenEditUsername);
                }
                let block_label = if peer.is_blocked {
                    tr::lng_profile_unblock_user()
                } else {
                    tr::lng_profile_block_user()
                };
                let block_icon = if peer.is_blocked {
                    &st::menu_icon_unblock
                } else {
                    &st::info_icon_block
                };
                if action_row(
                    ui,
                    block_icon,
                    &block_label,
                    None,
                    !peer.is_blocked,
                    false,
                    colors,
                ) {
                    action = Some(ProfileAction::ToggleBlock(peer.id));
                }
                if action_row(
                    ui,
                    &st::info_icon_report,
                    "Report Profile Photo",
                    None,
                    true,
                    false,
                    colors,
                ) {
                    action = Some(ProfileAction::ReportProfilePhoto);
                }
                if action_row(
                    ui,
                    &st::info_icon_delete_red,
                    &tr::lng_profile_delete_conversation(),
                    None,
                    true,
                    false,
                    colors,
                ) {
                    action = Some(ProfileAction::OpenDeleteOrLeave);
                }
            }
            InfoPeerKind::Bot => {
                let privacy_note = if peer.bot_reads_all_messages {
                    tr::lng_status_bot_reads_all()
                } else {
                    strings::PRIVACY_MODE_ENABLED.to_owned()
                };
                let cmd_badge = format!("{} {}", peer.bot_commands_count, strings::BOT_CMDS_SUFFIX);
                action_row(
                    ui,
                    &st::menu_icon_bot_commands,
                    &privacy_note,
                    Some(&cmd_badge),
                    false,
                    false,
                    colors,
                );
                if action_row(
                    ui,
                    &st::info_icon_share,
                    strings::SHARE_BOT,
                    None,
                    false,
                    false,
                    colors,
                ) {
                    action = Some(ProfileAction::OpenSharePeer);
                }
                let stop_label = if peer.is_blocked {
                    tr::lng_profile_restart_bot()
                } else {
                    tr::lng_profile_block_bot()
                };
                let stop_icon = if peer.is_blocked {
                    &st::menu_icon_restart_bot
                } else {
                    &st::info_icon_block
                };
                if action_row(
                    ui,
                    stop_icon,
                    &stop_label,
                    None,
                    !peer.is_blocked,
                    false,
                    colors,
                ) {
                    action = Some(ProfileAction::ToggleBlock(peer.id));
                }
                if action_row(
                    ui,
                    &st::info_icon_report,
                    &tr::lng_report_bot_title(),
                    None,
                    true,
                    false,
                    colors,
                ) {
                    action = Some(ProfileAction::OpenReport);
                }
            }
            InfoPeerKind::Group | InfoPeerKind::Supergroup => {
                if action_row(
                    ui,
                    &st::info_icon_share,
                    strings::SHARE_GROUP,
                    None,
                    false,
                    false,
                    colors,
                ) {
                    action = Some(ProfileAction::OpenSharePeer);
                }
                if action_row(
                    ui,
                    &st::info_icon_report,
                    &tr::lng_report_group_title(),
                    None,
                    true,
                    false,
                    colors,
                ) {
                    action = Some(ProfileAction::OpenReport);
                }
                if action_row(
                    ui,
                    &st::info_icon_leave,
                    &tr::lng_profile_leave_group(),
                    None,
                    true,
                    false,
                    colors,
                ) {
                    action = Some(ProfileAction::OpenDeleteOrLeave);
                }
            }
            InfoPeerKind::Channel => {
                if action_row(
                    ui,
                    &st::info_icon_share,
                    strings::SHARE_CHANNEL,
                    None,
                    false,
                    false,
                    colors,
                ) {
                    action = Some(ProfileAction::OpenSharePeer);
                }
                if action_row(
                    ui,
                    &st::info_icon_report,
                    &tr::lng_report_title(),
                    None,
                    true,
                    false,
                    colors,
                ) {
                    action = Some(ProfileAction::OpenReport);
                }
                if action_row(
                    ui,
                    &st::info_icon_leave,
                    &tr::lng_profile_leave_channel(),
                    None,
                    true,
                    false,
                    colors,
                ) {
                    action = Some(ProfileAction::OpenDeleteOrLeave);
                }
            }
            InfoPeerKind::SavedMessages => {}
        }

        action
    }
}

fn info_value_row(
    ui: &mut Ui,
    value: &str,
    caption: &str,
    leading_icon: Option<&Icon>,
    is_link: bool,
    colors: &dyn Colors,
) -> bool {
    let width = ui.available_width().max(180.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 46.0), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, colors.color("windowBgOver"));
    }
    if let Some(ic) = leading_icon {
        let icon_rect = Rect::from_center_size(
            Pos2::new(rect.min.x + 30.0, rect.center().y),
            Vec2::splat(20.0),
        );
        paint_style_icon(
            ui.painter(),
            icon_rect,
            *ic,
            &ColorsResolver(colors),
            Some(resolve_color_ref(st::info_icon_fg, colors)),
        );
    }
    let text_x = rect.min.x + 56.0;
    let val_fg = if is_link {
        colors.color("windowActiveTextFg")
    } else {
        colors.color("windowFg")
    };
    let clip = Rect::from_min_max(
        Pos2::new(text_x, rect.min.y),
        Pos2::new(rect.max.x - 14.0, rect.max.y),
    );
    let p = ui.painter().with_clip_rect(clip);
    p.text(
        Pos2::new(text_x, rect.min.y + 7.0),
        Align2::LEFT_TOP,
        value,
        font_id_from_style(st::normal_font),
        val_fg,
    );
    p.text(
        Pos2::new(text_x, rect.min.y + 26.0),
        Align2::LEFT_TOP,
        caption,
        FontId::new(11.5, family_regular()),
        colors.color("windowSubTextFg"),
    );
    resp.clicked()
}

fn info_multiline_row(
    ui: &mut Ui,
    value: &str,
    caption: &str,
    leading_icon: Option<&Icon>,
    colors: &dyn Colors,
) {
    let width = ui.available_width().max(180.0);
    let text_w = (width - 72.0).max(100.0);
    let galley = ui.painter().layout(
        value.to_owned(),
        font_id_from_style(st::normal_font),
        colors.color("windowFg"),
        text_w,
    );
    let total_h = (galley.size().y + 28.0).max(46.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, total_h), Sense::hover());
    if let Some(ic) = leading_icon {
        let icon_rect = Rect::from_center_size(
            Pos2::new(rect.min.x + 30.0, rect.min.y + 18.0),
            Vec2::splat(20.0),
        );
        paint_style_icon(
            ui.painter(),
            icon_rect,
            *ic,
            &ColorsResolver(colors),
            Some(resolve_color_ref(st::info_icon_fg, colors)),
        );
    }
    let text_x = rect.min.x + 56.0;
    ui.painter().galley(
        Pos2::new(text_x, rect.min.y + 6.0),
        galley.clone(),
        colors.color("windowFg"),
    );
    ui.painter().text(
        Pos2::new(text_x, rect.min.y + 10.0 + galley.size().y),
        Align2::LEFT_TOP,
        caption,
        FontId::new(11.5, family_regular()),
        colors.color("windowSubTextFg"),
    );
}

fn toggle_action_row(ui: &mut Ui, label: &str, checked: &mut bool, colors: &dyn Colors) -> bool {
    let width = ui.available_width().max(180.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 44.0), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, colors.color("windowBgOver"));
    }
    let icon_rect = Rect::from_center_size(
        Pos2::new(rect.min.x + 30.0, rect.center().y),
        Vec2::splat(20.0),
    );
    paint_style_icon(
        ui.painter(),
        icon_rect,
        st::menu_icon_notifications,
        &ColorsResolver(colors),
        Some(resolve_color_ref(st::info_icon_fg, colors)),
    );
    ui.painter().text(
        Pos2::new(rect.min.x + 56.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font_id_from_style(st::info_profile_button.style.font),
        colors.color("windowFg"),
    );

    let tog_st = ToggleStyle::default_toggle(colors);
    let tsize = Toggle::toggle_size(&tog_st);
    let tog_rect = Rect::from_min_size(
        Pos2::new(
            rect.max.x - tsize.x - 18.0,
            rect.center().y - tsize.y * 0.5 - 4.0,
        ),
        Vec2::new(tsize.x, tsize.y + 8.0),
    );
    let mut t_ui = ui.new_child(egui::UiBuilder::new().max_rect(tog_rect));
    let toggle_resp = Toggle::new(checked, tog_st).show(&mut t_ui);
    if resp.clicked() && !toggle_resp.clicked() {
        *checked = !*checked;
        return true;
    }
    toggle_resp.changed()
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
    let width = ui.available_width().max(180.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 42.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::accesskit::Role::Button, true, label));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, colors.color("windowBgOver"));
    }
    let fg = if attention {
        colors.color("attentionButtonFg")
    } else if accent {
        colors.color("windowActiveTextFg")
    } else {
        colors.color("windowFg")
    };
    let icon_fg = if attention {
        colors.color("attentionButtonFg")
    } else if accent {
        colors.color("windowActiveTextFg")
    } else {
        resolve_color_ref(st::info_icon_fg, colors)
    };
    let icon_rect = Rect::from_center_size(
        Pos2::new(rect.min.x + 30.0, rect.center().y),
        Vec2::splat(20.0),
    );
    paint_style_icon(
        ui.painter(),
        icon_rect,
        *icon,
        &ColorsResolver(colors),
        Some(icon_fg),
    );

    ui.painter().text(
        Pos2::new(rect.min.x + 56.0, rect.center().y),
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
            if attention {
                colors.color("attentionButtonFg")
            } else {
                colors.color("windowActiveTextFg")
            },
        );
    }

    resp.clicked()
}
