//! Edit Peer Info, Group Permissions (`ChatBannedRights` + Slow Mode),
//! Administrators (`ChatAdminRights`), Linked Discussion Group, and Auto-Delete Timer boxes.
//!
//! Port origin: `Telegram/SourceFiles/boxes/peers/edit_peer_info_box.cpp`,
//! `Telegram/SourceFiles/boxes/peers/edit_peer_permissions_box.cpp`,
//! `Telegram/SourceFiles/boxes/peers/edit_participant_box.cpp`,
//! `Telegram/SourceFiles/boxes/peers/edit_peer_type_box.cpp`,
//! `Telegram/SourceFiles/boxes/peers/edit_discussion_link_box.cpp`,
//! and `Telegram/SourceFiles/menu/menu_ttl.cpp`.

use egui::{Align2, CornerRadius, CursorIcon, FontId, Pos2, Rect, Sense, Ui, Vec2};
use tg_data::rights::{ChatAdminRights, ChatBannedRights};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold, font_id_from_style};
use tg_ui::icons::paint_style_icon;
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{
    CheckboxStyle, Colors, FlatLabelStyle, InputFieldStyle, RadioStyle, SettingsSliderStyle,
    ToggleStyle,
};
use tg_ui::tg_style::{Icon, st};
use tg_ui::theme::ColorsResolver;
use tg_ui::userpic::EmptyUserpic;
use tg_ui::widgets::checkbox::{Radio, Toggle};
use tg_ui::widgets::input_field::InputField;
use tg_ui::widgets::labels::{FlatLabel, divider_bar};
use tg_ui::widgets::sliders::SettingsSlider;

use crate::boxes::create_peer::{ChannelVisibility, photo_picker_circle};
use crate::boxes::edit_peer_extras::format_sticker_set_badge;
use crate::boxes::strings;
use crate::info::backend::{
    ChannelManagementState, InfoBackend, InfoPeerKind, MemberEntry, MemberRole, PeerProfile,
    UpdatePeerInfoRequest,
};
use crate::info::values::{
    AUTO_DELETE_TTL_PRESETS, MAX_ADMIN_TITLE_LENGTH, MAX_CHANNEL_DESCRIPTION,
    MAX_GROUP_CHANNEL_TITLE, MAX_USERNAME_LENGTH, UsernameAvailability, format_auto_delete_ttl,
    format_slowmode, slowmode_delay_by_index, slowmode_index_for_seconds,
};
use tg_app_core::stand_in::types::PeerId;

/// Sticky group-level send media rights mask matching `NestedRestrictionLabelsList` in `edit_peer_permissions_box.cpp`.
pub const STICKERS_AND_GIFS_RIGHTS: ChatBannedRights = ChatBannedRights::SEND_STICKERS
    .union(ChatBannedRights::SEND_GIFS)
    .union(ChatBannedRights::SEND_GAMES)
    .union(ChatBannedRights::SEND_INLINE);

/// Combined mask of the 10 individual media rights inside the "Send media" group.
pub const ALL_MEDIA_RIGHTS: ChatBannedRights = ChatBannedRights::SEND_PHOTOS
    .union(ChatBannedRights::SEND_VIDEOS)
    .union(ChatBannedRights::SEND_ROUNDVIDEOS)
    .union(ChatBannedRights::SEND_AUDIOS)
    .union(ChatBannedRights::SEND_VOICES)
    .union(ChatBannedRights::SEND_DOCS)
    .union(STICKERS_AND_GIFS_RIGHTS)
    .union(ChatBannedRights::EMBED_LINKS)
    .union(ChatBannedRights::SEND_POLLS)
    .union(ChatBannedRights::SEND_REACTIONS);

/// Combined mask of all story admin rights (`PostStories | EditStories | DeleteStories`).
pub const MANAGE_STORIES_ADMIN_RIGHTS: ChatAdminRights = ChatAdminRights::POST_STORIES
    .union(ChatAdminRights::EDIT_STORIES)
    .union(ChatAdminRights::DELETE_STORIES);

/// Returns `true` if `flag` is **allowed** (i.e. NOT banned) in `banned`.
///
/// In Telegram's `chatBannedRights`, a set bit means the action is forbidden.
#[must_use]
pub const fn is_permission_allowed(banned: ChatBannedRights, flag: ChatBannedRights) -> bool {
    !banned.intersects(flag)
}

/// Sets whether `flag` is allowed (`true` clears the banned bits; `false` sets them).
pub fn set_permission_allowed(
    banned: &mut ChatBannedRights,
    flag: ChatBannedRights,
    allowed: bool,
) {
    if allowed {
        banned.remove(flag);
    } else {
        banned.insert(flag);
    }
}

/// Counts `(allowed, total)` across the standard group permission rows for summary badges.
#[must_use]
pub fn count_allowed_permissions(banned: ChatBannedRights, is_forum: bool) -> (usize, usize) {
    let mut rows = vec![
        ChatBannedRights::SEND_PLAIN,
        ChatBannedRights::SEND_PHOTOS,
        ChatBannedRights::SEND_VIDEOS,
        ChatBannedRights::SEND_ROUNDVIDEOS,
        ChatBannedRights::SEND_AUDIOS,
        ChatBannedRights::SEND_VOICES,
        ChatBannedRights::SEND_DOCS,
        STICKERS_AND_GIFS_RIGHTS,
        ChatBannedRights::EMBED_LINKS,
        ChatBannedRights::SEND_POLLS,
        ChatBannedRights::SEND_REACTIONS,
        ChatBannedRights::INVITE_USERS,
        ChatBannedRights::PIN_MESSAGES,
        ChatBannedRights::CHANGE_INFO,
    ];
    if is_forum {
        rows.push(ChatBannedRights::MANAGE_TOPICS);
    }
    let allowed = rows
        .iter()
        .filter(|&&flag| is_permission_allowed(banned, flag))
        .count();
    (allowed, rows.len())
}

/// Minimum members required to enable aggressive anti-spam (`telegram_antispam_group_size_min`,
/// `menu_antispam_validator.cpp:35-39`).
pub const ANTISPAM_MIN_MEMBERS: u32 = 100;

/// Sub-box navigation actions emitted from [`EditPeerInfoBoxState::show_body`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditPeerSubAction {
    /// Open Photo Crop box.
    OpenPhotoCrop,
    /// Open Permissions + Slow Mode sub-box.
    OpenPermissions,
    /// Open Administrators list sub-box.
    OpenAdministrators,
    /// Open Invite Links manager sub-box (`EditInviteLinksBox`).
    OpenInviteLinks,
    /// Open Join Requests approval sub-box (`JoinRequestsBox`).
    OpenJoinRequests,
    /// Open Peer Color & Emoji Background sub-box (`EditPeerColorBox`).
    OpenPeerColor,
    /// Open Emoji Userpic Builder sub-box (`info/userpic/*`).
    OpenEmojiUserpic,
    /// Open Allowed Reactions sub-box (`EditPeerReactionsBox`).
    OpenReactions,
    /// Open Linked Discussion Group picker sub-box.
    OpenLinkedDiscussion,
    /// Open Auto-Delete Timer sub-box.
    OpenAutoDelete,
    /// Open Group Welcome Messages editor (`history_view_welcome_messages_section`).
    OpenWelcomeMessages,
    /// Open linked Community overview or Create Community wizard (`CommunityBox`).
    OpenCommunity,
    /// Open Chat History Visibility for New Members (`EditPeerHistoryVisibilityBox`).
    OpenHistoryVisibility,
    /// Open Group Stickers & Custom Emoji Pack selector (`EditGroupStickersBox`).
    OpenGroupStickers,
    /// Open two-step Convert to Broadcast Group confirmation (`channels.convertToGigagroup`).
    ConvertToGigagroup,
    /// Open Deactivate All Collectible Usernames confirmation (`channels.deactivateAllUsernames`).
    DeactivateAllUsernames,
}

/// State for editing a group, channel, or profile (`EditPeerInfoBox`).
#[derive(Debug, Clone)]
pub struct EditPeerInfoBoxState {
    /// Target peer identifier.
    pub peer_id: PeerId,
    /// Peer category.
    pub kind: InfoPeerKind,
    /// Editable title / display name.
    pub title: String,
    /// Editable bio / description.
    pub description: String,
    /// Public vs Private type.
    pub visibility: ChannelVisibility,
    /// Public username without `@`.
    pub username: String,
    /// Live username availability status.
    pub username_status: UsernameAvailability,
    /// Restrict saving/forwarding content.
    pub no_forwards: bool,
    /// Aggressive Anti-Spam toggle state (`channels.toggleAntiSpam`).
    pub anti_spam_enabled: bool,
    /// Whether `anti_spam_enabled` has been synced from `InfoBackend`.
    pub anti_spam_loaded: bool,
    /// Optional anti-spam notice (e.g. member-count threshold warning).
    pub anti_spam_notice: Option<String>,
    /// WP-65/WP-69 cached channel & supergroup management flags.
    pub mgmt: ChannelManagementState,
    /// Whether `mgmt` has been synced from `InfoBackend::channel_management_state`.
    pub mgmt_loaded: bool,
    /// Draft text for Paid Messages price in Telegram Stars (`0..=10000`).
    pub paid_stars_draft: String,
    /// Inline status/error message for WP-69 management toggles.
    pub mgmt_notice: Option<String>,
    /// Whether a custom photo was updated via crop box.
    pub has_custom_photo: bool,
    /// Avatar palette index.
    pub color_index: u8,
    /// Validation error text.
    pub error: Option<String>,
}

impl EditPeerInfoBoxState {
    /// Initializes an editor from an existing [`PeerProfile`].
    #[must_use]
    pub fn from_profile(peer: &PeerProfile) -> Self {
        let username = peer.editable_username().unwrap_or_default().to_owned();
        let visibility = if peer.is_public {
            ChannelVisibility::Public
        } else {
            ChannelVisibility::Private
        };
        Self {
            peer_id: peer.id,
            kind: peer.kind,
            title: peer.title.clone(),
            description: peer.bio.clone(),
            visibility,
            username,
            username_status: UsernameAvailability::Empty,
            no_forwards: peer.no_forwards,
            anti_spam_enabled: false,
            anti_spam_loaded: false,
            anti_spam_notice: None,
            mgmt: ChannelManagementState {
                is_forum: peer.is_forum,
                ..ChannelManagementState::default()
            },
            mgmt_loaded: false,
            paid_stars_draft: "0".to_owned(),
            mgmt_notice: None,
            has_custom_photo: false,
            color_index: peer.color_index,
            error: None,
        }
    }

    /// Synchronizes [`Self::mgmt`] from `backend.channel_management_state(self.peer_id)`.
    pub fn sync_management_state(&mut self, backend: &dyn InfoBackend) {
        if let Some(st) = backend.channel_management_state(self.peer_id) {
            self.paid_stars_draft = st.stars_per_message.max(0).to_string();
            self.mgmt = st;
        }
        self.mgmt_loaded = true;
    }

    /// Toggles forum topics mode (`channels.toggleForum`) and updates cached state.
    pub fn try_toggle_forum(
        &mut self,
        enabled: bool,
        tabs: bool,
        has_linked_discussion: bool,
        backend: &mut dyn InfoBackend,
    ) -> Result<(), String> {
        if enabled && has_linked_discussion {
            let msg = tr::lng_forum_topics_no_discussion();
            self.mgmt_notice = Some(msg.clone());
            return Err(msg);
        }
        match backend.toggle_channel_forum(self.peer_id, enabled, enabled && tabs) {
            Ok(()) => {
                self.mgmt.is_forum = enabled;
                self.mgmt.forum_tabs = enabled && tabs;
                if enabled {
                    self.mgmt.pre_history_hidden = false;
                }
                self.mgmt_notice = None;
                Ok(())
            }
            Err(err) => {
                self.mgmt_notice = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Toggles viewing a forum as flat messages (`channels.toggleViewForumAsMessages`).
    pub fn try_toggle_view_as_messages(
        &mut self,
        as_messages: bool,
        backend: &mut dyn InfoBackend,
    ) -> Result<(), String> {
        match backend.toggle_view_forum_as_messages(self.peer_id, as_messages) {
            Ok(()) => {
                self.mgmt.view_forum_as_messages = as_messages;
                self.mgmt_notice = None;
                Ok(())
            }
            Err(err) => {
                self.mgmt_notice = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Toggles hiding the group members list (`channels.toggleParticipantsHidden`).
    pub fn try_toggle_participants_hidden(
        &mut self,
        hidden: bool,
        backend: &mut dyn InfoBackend,
    ) -> Result<(), String> {
        match backend.toggle_participants_hidden(self.peer_id, hidden) {
            Ok(()) => {
                self.mgmt.participants_hidden = hidden;
                self.mgmt_notice = None;
                Ok(())
            }
            Err(err) => {
                self.mgmt_notice = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Updates boosts needed to bypass group restrictions (`channels.setBoostsToUnblockRestrictions`).
    pub fn try_set_boosts_unrestrict(
        &mut self,
        boosts: u32,
        backend: &mut dyn InfoBackend,
    ) -> Result<(), String> {
        let clamped = boosts.min(5);
        match backend.set_boosts_to_unblock_restrictions(self.peer_id, clamped) {
            Ok(()) => {
                self.mgmt.boosts_unrestrict = clamped;
                self.mgmt_notice = None;
                Ok(())
            }
            Err(err) => {
                self.mgmt_notice = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Saves Stars-per-message pricing and (for channels) Direct Messages availability
    /// (`channels.updatePaidMessagesPrice`).
    pub fn try_apply_paid_messages_price(
        &mut self,
        broadcast_messages_allowed: bool,
        stars_per_message: i64,
        backend: &mut dyn InfoBackend,
    ) -> Result<(), String> {
        if !(0..=10_000).contains(&stars_per_message) {
            let msg = "Paid message price must be between 0 and 10,000 Stars.".to_owned();
            self.mgmt_notice = Some(msg.clone());
            return Err(msg);
        }
        match backend.update_channel_paid_messages_price(
            self.peer_id,
            broadcast_messages_allowed,
            stars_per_message,
        ) {
            Ok(()) => {
                self.mgmt.broadcast_messages_allowed = broadcast_messages_allowed;
                self.mgmt.stars_per_message = stars_per_message;
                self.paid_stars_draft = stars_per_message.to_string();
                self.mgmt_notice = None;
                Ok(())
            }
            Err(err) => {
                self.mgmt_notice = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Toggles channel post auto-translation (`channels.toggleAutotranslation`).
    pub fn try_toggle_autotranslation(
        &mut self,
        enabled: bool,
        backend: &mut dyn InfoBackend,
    ) -> Result<(), String> {
        match backend.toggle_channel_autotranslation(self.peer_id, enabled) {
            Ok(()) => {
                self.mgmt.autotranslation = enabled;
                self.mgmt_notice = None;
                Ok(())
            }
            Err(err) => {
                self.mgmt_notice = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Attempts to toggle aggressive anti-spam (`channels.toggleAntiSpam`), enforcing
    /// [`ANTISPAM_MIN_MEMBERS`] (`menu_antispam_validator.cpp:85-114`).
    pub fn try_toggle_anti_spam(
        &mut self,
        enabled: bool,
        members_count: u32,
        backend: &mut dyn InfoBackend,
    ) -> Result<(), String> {
        if enabled && members_count < ANTISPAM_MIN_MEMBERS {
            self.anti_spam_enabled = false;
            let msg =
                format!("Aggressive Anti-Spam requires at least {ANTISPAM_MIN_MEMBERS} members.");
            self.anti_spam_notice = Some(msg.clone());
            return Err(msg);
        }
        match backend.toggle_channel_anti_spam(self.peer_id, enabled) {
            Ok(()) => {
                self.anti_spam_enabled = enabled;
                self.anti_spam_loaded = true;
                self.anti_spam_notice = None;
                Ok(())
            }
            Err(err) => {
                self.anti_spam_notice = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Saves edits back into `backend`.
    pub fn submit(&mut self, backend: &mut dyn InfoBackend) -> Result<(), String> {
        let is_public = self.visibility == ChannelVisibility::Public;
        if (self.kind.is_group() || self.kind.is_channel()) && is_public {
            self.username_status = backend.check_username(&self.username, Some(self.peer_id));
            if !self.username_status.is_valid_public() {
                let msg = self.username_status.message(&self.username);
                self.error = Some(msg.clone());
                return Err(msg);
            }
        }
        let req = UpdatePeerInfoRequest {
            title: self.title.trim().to_owned(),
            description: self.description.trim().to_owned(),
            is_public,
            username: is_public.then(|| self.username.trim().to_owned()),
            no_forwards: self.no_forwards,
        };
        match backend.update_peer_info(self.peer_id, req) {
            Ok(()) => {
                self.error = None;
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
        let heading = match self.kind {
            InfoPeerKind::Channel => strings::EDIT_CHANNEL_TITLE.to_owned(),
            InfoPeerKind::Group | InfoPeerKind::Supergroup => tr::lng_edit_group(),
            _ => strings::EDIT_INFO_TITLE.to_owned(),
        };
        let mut modal = BoxModal::new(id, heading)
            .width(st::box_wide_width.0 as f32 + 24.0)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::default_btn("save", tr::lng_settings_save()));
        modal.max_content_height = 500.0;
        modal
    }

    /// Renders the Edit Peer Info body.
    pub fn show_body(
        &mut self,
        ui: &mut Ui,
        peer: Option<&PeerProfile>,
        backend: &mut dyn InfoBackend,
        colors: &dyn Colors,
    ) -> Option<EditPeerSubAction> {
        // Reserve right gutter so right-aligned badges and wrapped labels clear the modal scrollbar.
        let safe_w = (ui.available_width() - 14.0).max(200.0);
        ui.set_max_width(safe_w);
        let mut sub_action = None;
        let input_st = InputFieldStyle::default_input(colors);

        // 1. Photo + Title
        ui.horizontal(|ui| {
            if photo_picker_circle(
                ui,
                &self.title,
                self.has_custom_photo,
                self.color_index,
                colors,
            ) {
                sub_action = Some(EditPeerSubAction::OpenPhotoCrop);
            }
            ui.add_space(14.0);
            ui.vertical(|ui| {
                ui.add_space(4.0);
                let title_placeholder = if self.kind.is_channel() {
                    strings::CHANNEL_NAME_PH
                } else if self.kind.is_group() {
                    strings::GROUP_NAME_PH
                } else {
                    strings::DISPLAY_NAME_PH
                };
                let title_err = self.error.is_some() && self.title.trim().is_empty();
                InputField::single_line(
                    "edit_peer_title",
                    &mut self.title,
                    title_placeholder,
                    input_st,
                )
                .max_length(MAX_GROUP_CHANNEL_TITLE)
                .error(title_err)
                .show(ui);
            });
        });

        ui.add_space(8.0);
        let desc_lbl = tr::lng_info_about_label();
        InputField::multi_line("edit_peer_desc", &mut self.description, &desc_lbl, input_st)
            .max_length(MAX_CHANNEL_DESCRIPTION)
            .show(ui);

        // 2. Group / Channel Type (Public vs Private + Username)
        if self.kind.is_group() || self.kind.is_channel() {
            ui.add_space(10.0);
            divider_bar(ui, 6.0, colors);
            ui.add_space(8.0);

            FlatLabel::plain(
                if self.kind.is_channel() {
                    strings::CHANNEL_TYPE_HEADER
                } else {
                    strings::GROUP_TYPE_HEADER
                },
                FlatLabelStyle::subsection_title(colors),
            )
            .id_salt("edit_type_hdr")
            .show(ui);

            let cb_st = CheckboxStyle::box_checkbox(colors);
            let rad_st = RadioStyle::default_radio(colors);
            ui.horizontal(|ui| {
                Radio::new(
                    &mut self.visibility,
                    ChannelVisibility::Public,
                    strings::VISIBILITY_PUBLIC,
                    cb_st,
                    rad_st,
                )
                .show(ui);
                ui.add_space(12.0);
                Radio::new(
                    &mut self.visibility,
                    ChannelVisibility::Private,
                    strings::VISIBILITY_PRIVATE,
                    cb_st,
                    rad_st,
                )
                .show(ui);
            });

            if self.visibility == ChannelVisibility::Public {
                let resp = InputField::single_line(
                    "edit_peer_username",
                    &mut self.username,
                    strings::CHANNEL_PUBLIC_LINK_PH,
                    input_st,
                )
                .max_length(MAX_USERNAME_LENGTH)
                .show(ui);
                if resp.changed() || !self.username.is_empty() {
                    self.username_status =
                        backend.check_username(&self.username, Some(self.peer_id));
                }
                if self.username_status != UsernameAvailability::Empty {
                    let fg = if self.username_status.is_valid_public() {
                        colors.color("boxTextFgGood")
                    } else {
                        colors.color("boxTextFgError")
                    };
                    ui.label(
                        egui::RichText::new(self.username_status.message(&self.username))
                            .font(FontId::new(12.0, family_regular()))
                            .color(fg),
                    );
                }
            }

            ui.add_space(6.0);
            Toggle::new(&mut self.no_forwards, ToggleStyle::default_toggle(colors))
                .with_label(strings::RESTRICT_SAVING_CONTENT, colors.color("windowFg"))
                .show(ui);

            // 3. Sub-editor navigation rows (Permissions, Admins, Linked Discussion, Auto-Delete)
            ui.add_space(8.0);
            divider_bar(ui, 6.0, colors);
            ui.add_space(4.0);

            if let Some(p) = peer {
                if p.kind.is_group() {
                    let (allowed, total) =
                        count_allowed_permissions(p.default_banned_rights, p.is_forum);
                    let badge = if p.slowmode_seconds > 0 {
                        format!(
                            "{allowed}/{total} · {}",
                            format_slowmode(p.slowmode_seconds)
                        )
                    } else {
                        format!("{allowed}/{total}")
                    };
                    if nav_setting_row(
                        ui,
                        st::menu_icon_permissions,
                        &tr::lng_manage_peer_permissions(),
                        &badge,
                        colors,
                    ) {
                        sub_action = Some(EditPeerSubAction::OpenPermissions);
                    }
                }

                let admins_badge = p.admins_count.to_string();
                if nav_setting_row(
                    ui,
                    st::menu_icon_admin,
                    &tr::lng_manage_peer_administrators(),
                    &admins_badge,
                    colors,
                ) {
                    sub_action = Some(EditPeerSubAction::OpenAdministrators);
                }

                if nav_setting_row(
                    ui,
                    st::menu_icon_link,
                    &tr::lng_manage_peer_invite_links(),
                    "",
                    colors,
                ) {
                    sub_action = Some(EditPeerSubAction::OpenInviteLinks);
                }

                let jr_summary = backend.join_requests_summary(p.id);
                let jr_badge = if jr_summary.count > 0 {
                    jr_summary.count.to_string()
                } else {
                    String::new()
                };
                if nav_setting_row(
                    ui,
                    st::info_icon_add_member,
                    &tr::lng_manage_peer_requests(),
                    &jr_badge,
                    colors,
                ) {
                    sub_action = Some(EditPeerSubAction::OpenJoinRequests);
                }

                if nav_setting_row(ui, st::menu_icon_palette, "Appearance & Color", "", colors) {
                    sub_action = Some(EditPeerSubAction::OpenPeerColor);
                }

                if nav_setting_row(
                    ui,
                    st::menu_icon_palette,
                    "Emoji Avatar Builder",
                    "",
                    colors,
                ) {
                    sub_action = Some(EditPeerSubAction::OpenEmojiUserpic);
                }

                if nav_setting_row(ui, st::menu_icon_star, "Reactions", "", colors) {
                    sub_action = Some(EditPeerSubAction::OpenReactions);
                }

                if p.kind.is_channel() {
                    let linked_badge = p
                        .linked_discussion_peer
                        .as_ref()
                        .map_or(strings::LINKED_GROUP_NONE, |(_, t)| t.as_str());
                    if nav_setting_row(
                        ui,
                        st::menu_icon_discussion,
                        &tr::lng_manage_discussion_group(),
                        linked_badge,
                        colors,
                    ) {
                        sub_action = Some(EditPeerSubAction::OpenLinkedDiscussion);
                    }
                }

                let ttl_badge = format_auto_delete_ttl(p.auto_delete_ttl_seconds);
                if nav_setting_row(
                    ui,
                    st::menu_icon_ttl,
                    &tr::lng_settings_ttl_title(),
                    &ttl_badge,
                    colors,
                ) {
                    sub_action = Some(EditPeerSubAction::OpenAutoDelete);
                }

                // Channel / Supergroup Aggressive Anti-Spam toggle (`channels.toggleAntiSpam`)
                if matches!(p.kind, InfoPeerKind::Supergroup | InfoPeerKind::Channel) {
                    if !self.anti_spam_loaded {
                        self.anti_spam_enabled = backend.channel_anti_spam_enabled(p.id);
                        self.anti_spam_loaded = true;
                    }
                    ui.add_space(6.0);
                    let mut next_antispam = self.anti_spam_enabled;
                    let locked = p.members_count < ANTISPAM_MIN_MEMBERS;
                    let label = if locked {
                        format!("Aggressive Anti-Spam ({ANTISPAM_MIN_MEMBERS}+)")
                    } else {
                        "Aggressive Anti-Spam".to_owned()
                    };
                    if Toggle::new(&mut next_antispam, ToggleStyle::default_toggle(colors))
                        .with_label(&label, colors.color("windowFg"))
                        .show(ui)
                        .changed()
                    {
                        let _ = self.try_toggle_anti_spam(next_antispam, p.members_count, backend);
                    }
                    if let Some(ref notice) = self.anti_spam_notice {
                        ui.label(
                            egui::RichText::new(notice)
                                .font(FontId::new(12.0, family_regular()))
                                .color(colors.color("boxTextFgError")),
                        );
                    }
                }

                // Welcome Messages (`history_view_welcome_messages_section`)
                if p.kind.is_group() {
                    let welcome_count =
                        backend.welcome_messages(p.id).map(|v| v.len()).unwrap_or(0);
                    let welcome_badge = if welcome_count > 0 {
                        welcome_count.to_string()
                    } else {
                        "Off".to_owned()
                    };
                    if nav_setting_row(
                        ui,
                        st::menu_icon_discussion,
                        "Welcome Messages",
                        &welcome_badge,
                        colors,
                    ) {
                        sub_action = Some(EditPeerSubAction::OpenWelcomeMessages);
                    }
                }

                // Linked Community (`CommunityBox` / `CreateCommunityBox`)
                let comm_badge = backend
                    .community_for_peer(p.id)
                    .map(|c| c.title)
                    .unwrap_or_else(|| "None".to_owned());
                if nav_setting_row(ui, st::menu_icon_groups, "Community", &comm_badge, colors) {
                    sub_action = Some(EditPeerSubAction::OpenCommunity);
                }

                // -------------------------------------------------------------
                // WP-69: Channel & Supergroup Management Controls (WP-65 RPCs)
                // -------------------------------------------------------------
                if !self.mgmt_loaded {
                    self.sync_management_state(backend);
                } else if let Some(fresh) = backend.channel_management_state(p.id) {
                    self.mgmt.pre_history_hidden = fresh.pre_history_hidden;
                    self.mgmt.is_gigagroup = fresh.is_gigagroup;
                    self.mgmt.sticker_set = fresh.sticker_set;
                    self.mgmt.emoji_set = fresh.emoji_set;
                }

                ui.add_space(8.0);
                divider_bar(ui, 6.0, colors);
                ui.add_space(6.0);

                // A. Chat history for new members (private non-forum groups without linked channel)
                if p.kind.is_group()
                    && self.visibility == ChannelVisibility::Private
                    && !self.mgmt.is_forum
                    && p.linked_discussion_peer.is_none()
                {
                    let vis_badge = if self.mgmt.pre_history_hidden {
                        tr::lng_manage_history_visibility_hidden()
                    } else {
                        tr::lng_manage_history_visibility_shown()
                    };
                    if nav_setting_row(
                        ui,
                        st::menu_icon_discussion,
                        &tr::lng_manage_history_visibility_title(),
                        &vis_badge,
                        colors,
                    ) {
                        sub_action = Some(EditPeerSubAction::OpenHistoryVisibility);
                    }
                }

                // B. Forum Topics, Tabbed Layout & View as Messages (`channels.toggleForum` /
                //    `channels.toggleViewForumAsMessages`)
                if p.kind.is_group() {
                    let mut forum_on = self.mgmt.is_forum;
                    let topics_lbl = tr::lng_forum_topics_switch();
                    if Toggle::new(&mut forum_on, ToggleStyle::default_toggle(colors))
                        .with_label(&topics_lbl, colors.color("windowFg"))
                        .show(ui)
                        .changed()
                    {
                        let _ = self.try_toggle_forum(
                            forum_on,
                            self.mgmt.forum_tabs,
                            p.linked_discussion_peer.is_some(),
                            backend,
                        );
                    }
                    if self.mgmt.is_forum {
                        ui.horizontal_wrapped(|ui| {
                            ui.add_space(16.0);
                            let mut tabs_on = self.mgmt.forum_tabs;
                            if Toggle::new(&mut tabs_on, ToggleStyle::default_toggle(colors))
                                .with_label("Show Topics as Tabs", colors.color("windowFg"))
                                .show(ui)
                                .changed()
                            {
                                let _ = self.try_toggle_forum(
                                    true,
                                    tabs_on,
                                    p.linked_discussion_peer.is_some(),
                                    backend,
                                );
                            }
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.add_space(16.0);
                            let mut view_msgs = self.mgmt.view_forum_as_messages;
                            let view_lbl = tr::lng_forum_view_as_messages();
                            if Toggle::new(&mut view_msgs, ToggleStyle::default_toggle(colors))
                                .with_label(&view_lbl, colors.color("windowFg"))
                                .show(ui)
                                .changed()
                            {
                                let _ = self.try_toggle_view_as_messages(view_msgs, backend);
                            }
                        });
                    }

                    // C. Hide Members (`channels.toggleParticipantsHidden`)
                    ui.add_space(4.0);
                    let mut hide_members = self.mgmt.participants_hidden;
                    let hide_lbl = tr::lng_profile_hide_participants();
                    if Toggle::new(&mut hide_members, ToggleStyle::default_toggle(colors))
                        .with_label(&hide_lbl, colors.color("windowFg"))
                        .show(ui)
                        .changed()
                    {
                        let _ = self.try_toggle_participants_hidden(hide_members, backend);
                    }
                    FlatLabel::plain(
                        &tr::lng_profile_hide_participants_about(),
                        FlatLabelStyle::sub_text(colors),
                    )
                    .id_salt("edit_hide_members_about")
                    .show(ui);

                    // D. Boosts to Unrestrict (`channels.setBoostsToUnblockRestrictions`)
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            egui::RichText::new("Boosts to Unrestrict")
                                .font(font_id_from_style(st::semibold_font))
                                .color(colors.color("windowFg")),
                        );
                        let badge = if self.mgmt.boosts_unrestrict == 0 {
                            "Off".to_owned()
                        } else {
                            format!("{} boost(s)", self.mgmt.boosts_unrestrict)
                        };
                        ui.label(
                            egui::RichText::new(badge)
                                .font(FontId::new(12.0, family_semibold()))
                                .color(colors.color("windowActiveTextFg")),
                        );
                        if ui
                            .add_enabled(self.mgmt.boosts_unrestrict > 0, egui::Button::new("−"))
                            .on_hover_text("Decrease required boosts")
                            .clicked()
                        {
                            let next = self.mgmt.boosts_unrestrict.saturating_sub(1);
                            let _ = self.try_set_boosts_unrestrict(next, backend);
                        }
                        if ui
                            .add_enabled(self.mgmt.boosts_unrestrict < 5, egui::Button::new("+"))
                            .on_hover_text("Increase required boosts")
                            .clicked()
                        {
                            let next = (self.mgmt.boosts_unrestrict + 1).min(5);
                            let _ = self.try_set_boosts_unrestrict(next, backend);
                        }
                    });

                    // E. Group Stickers & Emoji Pack (`channels.setStickers` / `setEmojiStickers`)
                    ui.add_space(4.0);
                    let stickers_badge = format_sticker_set_badge(self.mgmt.sticker_set.as_ref());
                    if nav_setting_row(
                        ui,
                        st::menu_icon_star,
                        &tr::lng_group_stickers(),
                        &stickers_badge,
                        colors,
                    ) {
                        sub_action = Some(EditPeerSubAction::OpenGroupStickers);
                    }
                }

                // F. Channel Post Auto-Translation (`channels.toggleAutotranslation`)
                if p.kind.is_channel() {
                    let mut auto_tr = self.mgmt.autotranslation;
                    let tr_lbl = tr::lng_edit_autotranslate();
                    if Toggle::new(&mut auto_tr, ToggleStyle::default_toggle(colors))
                        .with_label(&tr_lbl, colors.color("windowFg"))
                        .show(ui)
                        .changed()
                    {
                        let _ = self.try_toggle_autotranslation(auto_tr, backend);
                    }
                }

                // G. Paid Messages Price & Channel Direct Messages (`channels.updatePaidMessagesPrice`)
                ui.add_space(6.0);
                FlatLabel::plain(
                    "Paid Messages (Stars)",
                    FlatLabelStyle::subsection_title(colors),
                )
                .id_salt("edit_paid_msgs_hdr")
                .show(ui);
                ui.add_space(4.0);
                if p.kind.is_channel() {
                    let mut dm_allowed = self.mgmt.broadcast_messages_allowed;
                    if Toggle::new(&mut dm_allowed, ToggleStyle::default_toggle(colors))
                        .with_label("Allow Direct Messages to Channel", colors.color("windowFg"))
                        .show(ui)
                        .changed()
                    {
                        let _ = self.try_apply_paid_messages_price(
                            dm_allowed,
                            self.mgmt.stars_per_message,
                            backend,
                        );
                    }
                }
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new("Stars per message:")
                            .font(FontId::new(12.5, family_regular()))
                            .color(colors.color("windowFg")),
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut self.paid_stars_draft)
                            .desired_width(68.0)
                            .hint_text("0"),
                    );
                    if ui.button("Apply Price").clicked() {
                        match self.paid_stars_draft.trim().parse::<i64>() {
                            Ok(stars) => {
                                let _ = self.try_apply_paid_messages_price(
                                    self.mgmt.broadcast_messages_allowed,
                                    stars,
                                    backend,
                                );
                            }
                            Err(_) => {
                                self.mgmt_notice =
                                    Some("Enter a whole number of Stars (0 to 10,000).".to_owned());
                            }
                        }
                    }
                });

                // H. Convert to Broadcast Group (`channels.convertToGigagroup`)
                if p.kind.is_group() && !self.mgmt.is_gigagroup {
                    ui.add_space(6.0);
                    if nav_setting_row(
                        ui,
                        st::menu_icon_groups,
                        &tr::lng_rights_gigagroup_convert(),
                        "200K+",
                        colors,
                    ) {
                        sub_action = Some(EditPeerSubAction::ConvertToGigagroup);
                    }
                }

                // I. Deactivate All Collectible Usernames (`channels.deactivateAllUsernames`)
                let active_collectibles = p
                    .usernames
                    .iter()
                    .filter(|u| !u.is_editable && u.is_active)
                    .count();
                if active_collectibles > 0 {
                    ui.add_space(6.0);
                    let btn = egui::Button::new(
                        egui::RichText::new(format!(
                            "Deactivate All Collectible Usernames ({active_collectibles})"
                        ))
                        .color(colors.color("attentionButtonFg")),
                    );
                    if ui.add(btn).clicked() {
                        sub_action = Some(EditPeerSubAction::DeactivateAllUsernames);
                    }
                }

                if let Some(ref notice) = self.mgmt_notice {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(notice)
                            .font(FontId::new(12.0, family_regular()))
                            .color(colors.color("boxTextFgError")),
                    );
                }
            }
        }

        if let Some(ref err) = self.error {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(err)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("boxTextFgError")),
            );
        }

        sub_action
    }
}

/// Group default permissions + Slow Mode editor (`EditPeerPermissionsBox`).
#[derive(Debug, Clone)]
pub struct EditPermissionsBoxState {
    /// Target group id.
    pub peer_id: PeerId,
    /// Whether this group has forum topics enabled.
    pub is_forum: bool,
    /// Current `ChatBannedRights` bitflags (`tg_data`).
    pub banned_rights: ChatBannedRights,
    /// Selected index into `SLOWMODE_DELAYS_SECONDS` (`0..8`).
    pub slowmode_index: usize,
    /// Whether the nested "Send media" section is expanded.
    pub media_expanded: bool,
}

impl EditPermissionsBoxState {
    /// Initializes from an existing [`PeerProfile`].
    #[must_use]
    pub fn from_profile(peer: &PeerProfile) -> Self {
        Self {
            peer_id: peer.id,
            is_forum: peer.is_forum,
            banned_rights: peer.default_banned_rights,
            slowmode_index: slowmode_index_for_seconds(peer.slowmode_seconds),
            media_expanded: true,
        }
    }

    /// Returns the currently selected slow-mode delay in seconds.
    #[must_use]
    pub fn slowmode_seconds(&self) -> u32 {
        slowmode_delay_by_index(self.slowmode_index)
    }

    /// Persists changes to `backend`.
    pub fn submit(&self, backend: &mut dyn InfoBackend) -> Result<(), String> {
        backend.update_default_permissions(
            self.peer_id,
            self.banned_rights,
            self.slowmode_seconds(),
        )
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        BoxModal::new(id, tr::lng_manage_peer_permissions())
            .width(st::box_wide_width.0 as f32 + 32.0)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::default_btn("save", tr::lng_settings_save()))
    }

    /// Renders all permission toggles and the 8-step Slow Mode slider.
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) {
        FlatLabel::plain(
            &tr::lng_rights_default_restrictions_header(),
            FlatLabelStyle::subsection_title(colors),
        )
        .id_salt("perm_hdr")
        .show(ui);
        ui.add_space(6.0);

        permission_toggle(
            ui,
            &tr::lng_rights_chat_send_text(),
            &mut self.banned_rights,
            ChatBannedRights::SEND_PLAIN,
            false,
            colors,
        );

        let mut all_media_allowed = is_permission_allowed(self.banned_rights, ALL_MEDIA_RIGHTS);
        if toggle_row_with_caption(
            ui,
            &tr::lng_rights_chat_send_media(),
            &mut all_media_allowed,
            colors,
        ) {
            set_permission_allowed(&mut self.banned_rights, ALL_MEDIA_RIGHTS, all_media_allowed);
        }

        // Expand/collapse media types row with real `st::menu_icon_collapse` / `st::menu_icon_expand`.
        if expand_toggle_row(
            ui,
            self.media_expanded,
            if self.media_expanded {
                strings::COLLAPSE_MEDIA_TYPES
            } else {
                strings::EXPAND_MEDIA_TYPES
            },
            colors,
        ) {
            self.media_expanded = !self.media_expanded;
        }

        if self.media_expanded {
            let media_items = [
                (tr::lng_rights_chat_photos(), ChatBannedRights::SEND_PHOTOS),
                (tr::lng_rights_chat_videos(), ChatBannedRights::SEND_VIDEOS),
                (
                    tr::lng_rights_chat_video_messages(),
                    ChatBannedRights::SEND_ROUNDVIDEOS,
                ),
                (tr::lng_rights_chat_music(), ChatBannedRights::SEND_AUDIOS),
                (
                    tr::lng_rights_chat_voice_messages(),
                    ChatBannedRights::SEND_VOICES,
                ),
                (tr::lng_rights_chat_files(), ChatBannedRights::SEND_DOCS),
                (tr::lng_rights_chat_stickers(), STICKERS_AND_GIFS_RIGHTS),
                (
                    tr::lng_rights_chat_send_links(),
                    ChatBannedRights::EMBED_LINKS,
                ),
                (
                    tr::lng_rights_chat_send_polls(),
                    ChatBannedRights::SEND_POLLS,
                ),
                (
                    tr::lng_rights_chat_send_reactions(),
                    ChatBannedRights::SEND_REACTIONS,
                ),
            ];
            for (label, flag) in media_items {
                permission_toggle(ui, &label, &mut self.banned_rights, flag, true, colors);
            }
        }

        ui.add_space(4.0);
        permission_toggle(
            ui,
            &tr::lng_rights_chat_add_members(),
            &mut self.banned_rights,
            ChatBannedRights::INVITE_USERS,
            false,
            colors,
        );
        if self.is_forum {
            permission_toggle(
                ui,
                &tr::lng_rights_group_add_topics(),
                &mut self.banned_rights,
                ChatBannedRights::MANAGE_TOPICS,
                false,
                colors,
            );
        }
        permission_toggle(
            ui,
            &tr::lng_rights_group_pin(),
            &mut self.banned_rights,
            ChatBannedRights::PIN_MESSAGES,
            false,
            colors,
        );
        permission_toggle(
            ui,
            &tr::lng_rights_group_info(),
            &mut self.banned_rights,
            ChatBannedRights::CHANGE_INFO,
            false,
            colors,
        );

        // Slow Mode (`kSlowmodeValues = 8` in `edit_peer_permissions_box.cpp`)
        ui.add_space(10.0);
        divider_bar(ui, 6.0, colors);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            FlatLabel::plain(
                &tr::lng_rights_slowmode_header(),
                FlatLabelStyle::subsection_title(colors),
            )
            .id_salt("slowmode_hdr")
            .show(ui);
            ui.label(
                egui::RichText::new(format_slowmode(self.slowmode_seconds()))
                    .font(font_id_from_style(st::semibold_font))
                    .color(colors.color("windowActiveTextFg")),
            );
        });
        ui.add_space(4.0);
        let off = tr::lng_rights_slowmode_off();
        SettingsSlider::new(
            "slowmode_slider",
            &mut self.slowmode_index,
            &[off.as_str(), "5s", "10s", "30s", "1m", "5m", "15m", "1h"],
            SettingsSliderStyle::default_settings_slider(colors),
        )
        .show(ui);
        FlatLabel::plain(
            &tr::lng_rights_slowmode_about(),
            FlatLabelStyle::sub_text(colors),
        )
        .id_salt("slowmode_hint")
        .show(ui);
    }
}

/// Administrators list box showing current admins and allowing promotion/editing.
#[derive(Debug, Clone)]
pub struct AdminsListBoxState {
    /// Target group/channel id.
    pub peer_id: PeerId,
}

/// Event emitted from [`AdminsListBoxState::show_body`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminsListAction {
    /// Clicked an admin row to edit their rights.
    EditAdmin(PeerId),
    /// Clicked "Add Administrator" to pick a member to promote.
    AddAdmin,
}

impl AdminsListBoxState {
    /// Creates a new administrators list modal state.
    #[must_use]
    pub const fn new(peer_id: PeerId) -> Self {
        Self { peer_id }
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        BoxModal::new(id, tr::lng_manage_peer_administrators())
            .width(st::box_wide_width.0 as f32 + 16.0)
            .with_left_button(BoxButton::default_btn("add_admin", strings::ADD_ADMIN_BTN))
            .add_button(BoxButton::default_btn("close", tr::lng_close()))
    }

    /// Renders all administrators for the group/channel.
    pub fn show_body(
        &self,
        ui: &mut Ui,
        members: &[MemberEntry],
        colors: &dyn Colors,
    ) -> Option<AdminsListAction> {
        let mut action = None;
        let admins: Vec<&MemberEntry> = members
            .iter()
            .filter(|m| m.role.is_admin_or_owner())
            .collect();

        for admin in admins {
            let width = ui.available_width().max(180.0);
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 52.0), Sense::click());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                ui.painter()
                    .rect_filled(rect, CornerRadius::same(6), colors.color("windowBgOver"));
            }
            let av_rect = Rect::from_min_size(
                Pos2::new(rect.min.x + 6.0, rect.center().y - 18.0),
                Vec2::splat(36.0),
            );
            EmptyUserpic::for_color_index(admin.color_index, &admin.name, colors)
                .paint_circle(ui.painter(), av_rect);

            let tx = av_rect.max.x + 12.0;
            ui.painter().text(
                Pos2::new(tx, rect.min.y + 8.0),
                Align2::LEFT_TOP,
                &admin.name,
                font_id_from_style(st::semibold_font),
                colors.color("windowBoldFg"),
            );
            let role_desc = match &admin.role {
                MemberRole::Owner { .. } => strings::ROLE_OWNER_TITLE.to_owned(),
                MemberRole::Admin { rights, .. } => format!(
                    "{} · {} {}",
                    strings::ADMIN_PERMISSIONS_PREFIX,
                    rights.bits().count_ones(),
                    strings::ADMIN_PERMISSIONS_SUFFIX
                ),
                _ => strings::ROLE_MEMBER_TITLE.to_owned(),
            };
            ui.painter().text(
                Pos2::new(tx, rect.min.y + 27.0),
                Align2::LEFT_TOP,
                role_desc,
                FontId::new(12.0, family_regular()),
                colors.color("windowSubTextFg"),
            );
            if let Some(badge) = admin.role.badge_label() {
                ui.painter().text(
                    Pos2::new(rect.max.x - 10.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    badge,
                    FontId::new(11.5, family_semibold()),
                    colors.color("windowActiveTextFg"),
                );
            }
            if resp.clicked() {
                action = Some(AdminsListAction::EditAdmin(admin.peer_id));
            }
        }
        action
    }
}

/// Administrator rights editor box (`EditAdminBox` in `edit_participant_box.cpp`).
#[derive(Debug, Clone)]
pub struct EditAdminRightsBoxState {
    /// Owning group or channel id.
    pub peer_id: PeerId,
    /// Target user id.
    pub user_id: PeerId,
    /// Display name of the user.
    pub user_name: String,
    /// Status text of the user.
    pub user_status: String,
    /// Avatar color index.
    pub user_color_index: u8,
    /// Whether the target peer is a group (vs broadcast channel).
    pub is_group: bool,
    /// Whether the target group is a forum.
    pub is_forum: bool,
    /// Whether the target user is the group owner.
    pub is_owner: bool,
    /// Granted `ChatAdminRights` bitflags (`tg_data`).
    pub rights: ChatAdminRights,
    /// Optional custom title (`0..=16` chars).
    pub custom_title: String,
    /// Validation error message.
    pub error: Option<String>,
}

impl EditAdminRightsBoxState {
    /// Initializes an admin rights editor for `member` inside `peer`.
    #[must_use]
    pub fn from_member(peer: &PeerProfile, member: &MemberEntry) -> Self {
        let is_owner = matches!(member.role, MemberRole::Owner { .. });
        let initial_rights = if member.role.is_admin_or_owner() {
            member.role.admin_rights()
        } else if peer.kind.is_channel() {
            ChatAdminRights::CHANGE_INFO
                | ChatAdminRights::POST_MESSAGES
                | ChatAdminRights::EDIT_MESSAGES
                | ChatAdminRights::DELETE_MESSAGES
        } else {
            ChatAdminRights::CHANGE_INFO
                | ChatAdminRights::DELETE_MESSAGES
                | ChatAdminRights::BAN_USERS
                | ChatAdminRights::INVITE_USERS
                | ChatAdminRights::PIN_MESSAGES
        };
        Self {
            peer_id: peer.id,
            user_id: member.peer_id,
            user_name: member.name.clone(),
            user_status: member.status_text.clone(),
            user_color_index: member.color_index,
            is_group: peer.kind.is_group(),
            is_forum: peer.is_forum,
            is_owner,
            rights: initial_rights,
            custom_title: member.role.custom_title().to_owned(),
            error: None,
        }
    }

    /// Saves updated admin rights and custom title to `backend`.
    pub fn submit(&mut self, backend: &mut dyn InfoBackend) -> Result<(), String> {
        match backend.update_admin_rights(
            self.peer_id,
            self.user_id,
            self.rights,
            self.custom_title.clone(),
        ) {
            Ok(()) => {
                self.error = None;
                Ok(())
            }
            Err(err) => {
                self.error = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Dismisses the administrator (clears all admin rights).
    pub fn dismiss_admin(&mut self, backend: &mut dyn InfoBackend) -> Result<(), String> {
        self.rights = ChatAdminRights::empty();
        self.custom_title.clear();
        self.submit(backend)
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        let mut modal = BoxModal::new(id, tr::lng_rights_edit_admin())
            .width(st::box_wide_width.0 as f32 + 24.0)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::default_btn("save", tr::lng_settings_save()));
        if !self.is_owner {
            modal = modal.with_left_button(BoxButton::attention("dismiss", strings::DISMISS_BTN));
        }
        modal
    }

    /// Renders the admin cover, `ChatAdminRights` toggles, and custom title field.
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) {
        ui.horizontal(|ui| {
            let (av_rect, _) = ui.allocate_exact_size(Vec2::splat(48.0), Sense::hover());
            EmptyUserpic::for_color_index(self.user_color_index, &self.user_name, colors)
                .paint_circle(ui.painter(), av_rect);
            ui.add_space(10.0);
            ui.vertical(|ui| {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(&self.user_name)
                        .font(font_id_from_style(st::semibold_font))
                        .color(colors.color("windowBoldFg")),
                );
                ui.label(
                    egui::RichText::new(&self.user_status)
                        .font(FontId::new(12.0, family_regular()))
                        .color(colors.color("windowSubTextFg")),
                );
            });
        });

        ui.add_space(10.0);
        FlatLabel::plain(
            &tr::lng_rights_edit_admin_header(),
            FlatLabelStyle::subsection_title(colors),
        )
        .id_salt("admin_rights_hdr")
        .show(ui);
        ui.add_space(4.0);

        if self.is_group {
            admin_flag_toggle(
                ui,
                &tr::lng_rights_group_info(),
                &mut self.rights,
                ChatAdminRights::CHANGE_INFO,
                !self.is_owner,
                colors,
            );
            admin_flag_toggle(
                ui,
                &tr::lng_rights_group_delete(),
                &mut self.rights,
                ChatAdminRights::DELETE_MESSAGES,
                !self.is_owner,
                colors,
            );
            admin_flag_toggle(
                ui,
                &tr::lng_rights_group_ban(),
                &mut self.rights,
                ChatAdminRights::BAN_USERS,
                !self.is_owner,
                colors,
            );
            admin_flag_toggle(
                ui,
                &tr::lng_rights_group_invite_link(),
                &mut self.rights,
                ChatAdminRights::INVITE_USERS,
                !self.is_owner,
                colors,
            );
            admin_flag_toggle(
                ui,
                &tr::lng_rights_group_pin(),
                &mut self.rights,
                ChatAdminRights::PIN_MESSAGES,
                !self.is_owner,
                colors,
            );
            if self.is_forum {
                admin_flag_toggle(
                    ui,
                    &tr::lng_rights_group_topics(),
                    &mut self.rights,
                    ChatAdminRights::MANAGE_TOPICS,
                    !self.is_owner,
                    colors,
                );
            }
            admin_flag_toggle(
                ui,
                &tr::lng_rights_group_manage_calls(),
                &mut self.rights,
                ChatAdminRights::MANAGE_CALL,
                !self.is_owner,
                colors,
            );
            admin_flag_toggle(
                ui,
                &tr::lng_rights_channel_manage_stories(),
                &mut self.rights,
                MANAGE_STORIES_ADMIN_RIGHTS,
                !self.is_owner,
                colors,
            );
            admin_flag_toggle(
                ui,
                &tr::lng_rights_group_anonymous(),
                &mut self.rights,
                ChatAdminRights::ANONYMOUS,
                true,
                colors,
            );
            admin_flag_toggle(
                ui,
                &tr::lng_rights_add_admins(),
                &mut self.rights,
                ChatAdminRights::ADD_ADMINS,
                !self.is_owner,
                colors,
            );
        } else {
            admin_flag_toggle(
                ui,
                &tr::lng_rights_channel_info(),
                &mut self.rights,
                ChatAdminRights::CHANGE_INFO,
                !self.is_owner,
                colors,
            );
            admin_flag_toggle(
                ui,
                &tr::lng_rights_channel_post(),
                &mut self.rights,
                ChatAdminRights::POST_MESSAGES,
                !self.is_owner,
                colors,
            );
            admin_flag_toggle(
                ui,
                &tr::lng_rights_channel_edit(),
                &mut self.rights,
                ChatAdminRights::EDIT_MESSAGES,
                !self.is_owner,
                colors,
            );
            admin_flag_toggle(
                ui,
                &tr::lng_rights_channel_delete(),
                &mut self.rights,
                ChatAdminRights::DELETE_MESSAGES,
                !self.is_owner,
                colors,
            );
            admin_flag_toggle(
                ui,
                &tr::lng_rights_channel_manage_stories(),
                &mut self.rights,
                MANAGE_STORIES_ADMIN_RIGHTS,
                !self.is_owner,
                colors,
            );
            admin_flag_toggle(
                ui,
                &tr::lng_rights_group_invite_link(),
                &mut self.rights,
                ChatAdminRights::INVITE_USERS,
                !self.is_owner,
                colors,
            );
            admin_flag_toggle(
                ui,
                &tr::lng_rights_channel_manage_calls(),
                &mut self.rights,
                ChatAdminRights::MANAGE_CALL,
                !self.is_owner,
                colors,
            );
            admin_flag_toggle(
                ui,
                &tr::lng_rights_add_admins(),
                &mut self.rights,
                ChatAdminRights::ADD_ADMINS,
                !self.is_owner,
                colors,
            );
        }

        ui.add_space(10.0);
        InputField::single_line(
            "admin_custom_title",
            &mut self.custom_title,
            strings::CUSTOM_TITLE_OPTIONAL_PH,
            InputFieldStyle::default_input(colors),
        )
        .max_length(MAX_ADMIN_TITLE_LENGTH)
        .show(ui);

        if let Some(ref err) = self.error {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(err)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("boxTextFgError")),
            );
        }
    }

    /// Renders an optional "Transfer Ownership" row for eligible non-owner admins,
    /// returning `true` when clicked.
    pub fn show_transfer_ownership_row(&self, ui: &mut Ui, colors: &dyn Colors) -> bool {
        if self.is_owner {
            return false;
        }
        ui.add_space(8.0);
        divider_bar(ui, 6.0, colors);
        ui.add_space(4.0);
        let label = if self.is_group {
            "Transfer Group Ownership"
        } else {
            "Transfer Channel Ownership"
        };
        nav_setting_row(ui, st::menu_icon_admin, label, "", colors)
    }
}

/// Auto-Delete Timer modal box (`TTLBox` in `menu/menu_ttl.cpp`).
#[derive(Debug, Clone)]
pub struct AutoDeleteTimerBoxState {
    /// Target peer id.
    pub peer_id: PeerId,
    /// Currently selected TTL in seconds (`0` = Off).
    pub selected_ttl: u32,
}

impl AutoDeleteTimerBoxState {
    /// Creates an auto-delete timer modal initialized with `current_ttl`.
    #[must_use]
    pub const fn new(peer_id: PeerId, current_ttl: u32) -> Self {
        Self {
            peer_id,
            selected_ttl: current_ttl,
        }
    }

    /// Saves the selected TTL to `backend`.
    pub fn submit(&self, backend: &mut dyn InfoBackend) -> Result<(), String> {
        backend.set_auto_delete_ttl(self.peer_id, self.selected_ttl)
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        let mut modal = BoxModal::new(id, tr::lng_settings_ttl_title())
            .width(st::box_width.0 as f32 + 24.0)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::default_btn("save", tr::lng_settings_save()));
        if self.selected_ttl > 0 {
            modal = modal.with_left_button(BoxButton::default_btn(
                "disable",
                tr::lng_manage_messages_ttl_disable(),
            ));
        }
        modal
    }

    /// Renders the auto-delete timer radio options.
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) {
        FlatLabel::plain(strings::TTL_DIALOG_DESC, FlatLabelStyle::sub_text(colors))
            .id_salt("ttl_desc")
            .show(ui);
        ui.add_space(8.0);

        let cb_st = CheckboxStyle::box_checkbox(colors);
        let rad_st = RadioStyle::default_radio(colors);
        for &ttl in &AUTO_DELETE_TTL_PRESETS {
            let label = format_auto_delete_ttl(ttl);
            Radio::new(&mut self.selected_ttl, ttl, &label, cb_st, rad_st).show(ui);
        }
    }
}

fn permission_toggle(
    ui: &mut Ui,
    label: &str,
    banned: &mut ChatBannedRights,
    flag: ChatBannedRights,
    indented: bool,
    colors: &dyn Colors,
) {
    let mut allowed = is_permission_allowed(*banned, flag);
    let width = ui.available_width().max(180.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 34.0), Sense::click());
    let left_pad = if indented { 20.0 } else { 0.0 };
    ui.painter().text(
        Pos2::new(rect.min.x + left_pad, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        if indented {
            FontId::new(13.0, family_regular())
        } else {
            font_id_from_style(st::normal_font)
        },
        colors.color("windowFg"),
    );
    let tog_st = ToggleStyle::default_toggle(colors);
    let tsize = Toggle::toggle_size(&tog_st);
    let tog_rect = Rect::from_min_size(
        Pos2::new(
            rect.max.x - tsize.x - 2.0,
            rect.center().y - tsize.y * 0.5 - 4.0,
        ),
        Vec2::new(tsize.x, tsize.y + 8.0),
    );
    let mut t_ui = ui.new_child(egui::UiBuilder::new().max_rect(tog_rect));
    let t_resp = Toggle::new(&mut allowed, tog_st).show(&mut t_ui);
    if resp.clicked() && !t_resp.clicked() {
        allowed = !allowed;
    }
    set_permission_allowed(banned, flag, allowed);
}

fn toggle_row_with_caption(
    ui: &mut Ui,
    label: &str,
    checked: &mut bool,
    colors: &dyn Colors,
) -> bool {
    let width = ui.available_width().max(180.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 36.0), Sense::click());
    ui.painter().text(
        Pos2::new(rect.min.x, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font_id_from_style(st::semibold_font),
        colors.color("windowBoldFg"),
    );
    let tog_st = ToggleStyle::default_toggle(colors);
    let tsize = Toggle::toggle_size(&tog_st);
    let tog_rect = Rect::from_min_size(
        Pos2::new(
            rect.max.x - tsize.x - 2.0,
            rect.center().y - tsize.y * 0.5 - 4.0,
        ),
        Vec2::new(tsize.x, tsize.y + 8.0),
    );
    let mut t_ui = ui.new_child(egui::UiBuilder::new().max_rect(tog_rect));
    let t_resp = Toggle::new(checked, tog_st).show(&mut t_ui);
    if resp.clicked() && !t_resp.clicked() {
        *checked = !*checked;
        return true;
    }
    t_resp.changed()
}

fn expand_toggle_row(ui: &mut Ui, expanded: bool, label: &str, colors: &dyn Colors) -> bool {
    let width = ui.available_width().max(180.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 28.0), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), colors.color("windowBgOver"));
    }
    let icon = if expanded {
        st::menu_icon_collapse
    } else {
        st::menu_icon_expand
    };
    let ic_rect = Rect::from_center_size(
        Pos2::new(rect.min.x + 10.0, rect.center().y),
        Vec2::splat(16.0),
    );
    paint_style_icon(
        ui.painter(),
        ic_rect,
        icon,
        &ColorsResolver(colors),
        Some(colors.color("windowActiveTextFg")),
    );
    ui.painter().text(
        Pos2::new(rect.min.x + 24.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::new(12.5, family_semibold()),
        colors.color("windowActiveTextFg"),
    );
    resp.clicked()
}

fn admin_flag_toggle(
    ui: &mut Ui,
    label: &str,
    rights: &mut ChatAdminRights,
    flag: ChatAdminRights,
    enabled: bool,
    colors: &dyn Colors,
) {
    let mut has = rights.contains(flag);
    let width = ui.available_width().max(180.0);
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(width, 34.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    ui.painter().text(
        Pos2::new(rect.min.x, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font_id_from_style(st::normal_font),
        if enabled {
            colors.color("windowFg")
        } else {
            colors.color("windowSubTextFg")
        },
    );
    let tog_st = ToggleStyle::default_toggle(colors);
    let tsize = Toggle::toggle_size(&tog_st);
    let tog_rect = Rect::from_min_size(
        Pos2::new(
            rect.max.x - tsize.x - 2.0,
            rect.center().y - tsize.y * 0.5 - 4.0,
        ),
        Vec2::new(tsize.x, tsize.y + 8.0),
    );
    let mut t_ui = ui.new_child(egui::UiBuilder::new().max_rect(tog_rect));
    let t_resp = Toggle::new(&mut has, tog_st)
        .enabled(enabled)
        .show(&mut t_ui);
    if enabled && resp.clicked() && !t_resp.clicked() {
        has = !has;
    }
    if enabled {
        if has {
            rights.insert(flag);
        } else {
            rights.remove(flag);
        }
    }
}

fn nav_setting_row(ui: &mut Ui, icon: Icon, label: &str, value: &str, colors: &dyn Colors) -> bool {
    let width = ui.max_rect().width().min(ui.available_width()).max(180.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 40.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), colors.color("windowBgOver"));
    }
    let ic_rect = Rect::from_center_size(
        Pos2::new(rect.min.x + 14.0, rect.center().y),
        Vec2::splat(20.0),
    );
    paint_style_icon(
        ui.painter(),
        ic_rect,
        icon,
        &ColorsResolver(colors),
        Some(colors.color("menuIconFg")),
    );
    // The label keeps its full width (tdesktop's `SettingsButton` elides the
    // right-hand value, never the label); the value is elided to the space left.
    let label_font = font_id_from_style(st::normal_font);
    let label_w = ui
        .painter()
        .layout_no_wrap(
            label.to_owned(),
            label_font.clone(),
            colors.color("windowFg"),
        )
        .size()
        .x;
    let label_clip = Rect::from_min_max(
        Pos2::new(rect.min.x + 36.0, rect.min.y),
        Pos2::new(rect.max.x - 16.0, rect.max.y),
    );
    ui.painter().with_clip_rect(label_clip).text(
        Pos2::new(rect.min.x + 36.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        label_font,
        colors.color("windowFg"),
    );
    if !value.is_empty() {
        let val_font = FontId::new(12.5, family_regular());
        let fg = colors.color("windowActiveTextFg");
        let max_w = (rect.width() - 36.0 - label_w - 12.0 - 16.0).max(0.0);
        let measure = |s: &str| {
            ui.painter()
                .layout_no_wrap(s.to_owned(), val_font.clone(), fg)
                .size()
                .x
        };
        let shown = if measure(value) <= max_w {
            value.to_owned()
        } else {
            let chars: Vec<char> = value.chars().collect();
            let (mut lo, mut hi) = (0usize, chars.len());
            while lo < hi {
                let mid = (lo + hi).div_ceil(2);
                let candidate: String = chars[..mid].iter().collect::<String>() + "…";
                if measure(&candidate) <= max_w {
                    lo = mid;
                } else {
                    hi = mid - 1;
                }
            }
            chars[..lo].iter().collect::<String>() + "…"
        };
        ui.painter().text(
            Pos2::new(rect.max.x - 16.0, rect.center().y),
            Align2::RIGHT_CENTER,
            shown,
            val_font,
            fg,
        );
    }
    resp.clicked()
}
