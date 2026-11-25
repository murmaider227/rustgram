//! Peer profile / info column, shared media tabs, members list, and InfoBackend abstraction.
//!
//! Port origin: `Telegram/SourceFiles/info/` (`info_wrap_widget.cpp`, `info_top_bar.cpp`,
//! `profile/info_profile_cover.cpp`, `profile/info_profile_actions.cpp`,
//! `profile/info_profile_members.cpp`, `profile/info_profile_values.cpp`,
//! `media/info_media_list_widget.cpp`, `media/info_media_provider.cpp`,
//! and `common_groups/info_common_groups_widget.cpp`).

pub mod actions;
pub use tg_app_core::info::backend;
pub mod balance_pill;
pub mod birthday_effect;
pub mod colors;
pub mod communities_mock;
pub mod community;
pub mod cover;
pub mod downloads;
pub mod global_media;
pub mod media;
pub mod members;
pub mod mock;
pub mod peer_mgmt_mock;
pub mod profile_controls;
pub mod profile_extras_mock;
pub mod profile_tabs;
pub mod reactions_list;
pub mod similar_channels;
pub mod strings;
pub mod top_bar;
pub use tg_app_core::info::values;
pub mod wrap_widget;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_wp78;

pub use actions::{ProfileAction, ProfileActionsView};
pub use backend::{
    ChatAvailableReactionsMode, CommonGroupEntry, CreateChannelRequest, CreateGroupRequest,
    DownloadEntry, DownloadStatus, EmojiUserpicRequest, InfoBackend, InfoPeerKind,
    InviteImporterInfo, InviteImportersList, InviteLinkInfo, InviteLinkRequest, InviteLinksList,
    JoinRequestsSummary, LanguageEntry, MediaItem, MemberEntry, MemberRole, OwnershipTransferCheck,
    OwnershipTransferError, PeerColorSettings, PeerProfile, PeerReactionsConfig, PeerUsername,
    PeerVerificationStatus, PersonalChannelOption, ProfileBalanceInfo, ProfileMainTab,
    ProfilePeerGiftsPage, ProfilePhotoRef, ProfileStoriesPage, ProfileStoryAlbum, ProfileStoryItem,
    ReportReason, SavedMusicTrack, SharedMediaCounts, SharedMediaTab, SimilarChannelEntry,
    SimilarChannelsResult, TermsOfServiceData, UpdatePeerColorRequest, UpdatePeerInfoRequest,
    default_standard_reactions,
};
pub use balance_pill::{
    BalanceAmountParts, BalancePillClick, ProfileBalancePill, format_ton_balance_parts,
};
pub use birthday_effect::{
    BIRTHDAY_EFFECT_DURATION_SECS, CONFETTI_PALETTE, ProfileBirthdayEffect, birthday_age,
    current_utc_ymd, is_birthday_today,
};
pub use colors::InfoColors;
pub use cover::{CoverAction, ProfileCover};
pub use downloads::{DownloadsAction, DownloadsSection};
pub use global_media::{
    GLOBAL_MEDIA_PAGE_SIZE, GlobalMediaAction, GlobalMediaEntry, GlobalMediaPage,
    GlobalMediaSection,
};
pub use media::{MediaDateSection, MediaListAction, MediaListWidget, MediaProvider};
pub use members::{CommonGroupsSection, MembersAction, MembersSection};
pub use mock::{ForwardRecord, MockBackend, SubmittedReport, mock_peers};
pub use profile_controls::{
    PersonalChannelAction, PersonalChannelSection, ReportProfilePhotoDraft,
    ReportProfilePhotoOutcome, SuggestBirthdayDraft, SuggestBirthdayOutcome,
    can_set_main_profile_tab, ordered_shared_media_tabs, profile_main_tab_label,
    selectable_main_tabs_for_peer, shared_media_to_profile_main_tab,
};
pub use reactions_list::{
    PEER_LIST_PAGE_LIMIT, PollVotersAction, PollVotersWidget, ReactionsListAction,
    ReactionsListWidget,
};
pub use similar_channels::{SimilarChannelsAction, SimilarChannelsSection};
pub use top_bar::{INFO_TOP_BAR_HEIGHT, InfoTopBar, TopBarAction, TopBarConfig};
pub use values::{
    AUTO_DELETE_TTL_PRESETS, Birthday, ENABLE_SEARCH_MEMBERS_AFTER_COUNT, MAX_ADMIN_TITLE_LENGTH,
    MAX_CHANNEL_DESCRIPTION, MAX_GROUP_CHANNEL_TITLE, MAX_REPORT_COMMENT_LENGTH,
    MAX_USER_FIRST_LAST_NAME, MAX_USERNAME_LENGTH, MIN_USERNAME_LENGTH, SLOWMODE_DELAYS_SECONDS,
    UsernameAvailability, format_auto_delete_ttl, format_birthday, format_duration,
    format_file_size, format_members_status, format_phone, format_slowmode,
    format_subscribers_status, format_thousands, format_username_link, is_valid_phone,
    normalize_username, slowmode_delay_by_index, slowmode_index_for_seconds,
    validate_username_syntax,
};
pub use wrap_widget::{InfoSection, InfoWrapEvent, InfoWrapWidget};

use egui::Ui;
use tg_data::rights::ChatBannedRights;

use tg_app_core::core_types::window_types::InfoTab;
use tg_app_core::stand_in::strings::Strings;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::{PeerId, PeerKind, PeerSummary};

/// Events emitted by the stateless shell compatibility adapter [`InfoWidget`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfoAction {
    /// Close the third column or navigate Back when hosted in the main column.
    CloseOrBack,
    /// Toggle mute notifications for this peer.
    ToggleMute(PeerId),
    /// Jump straight to messaging this peer.
    OpenHistory(PeerId),
}

/// Shell compatibility renderer for the Peer Info view (`Info::WrapWidget`).
///
/// Preserves the signature consumed by `window::main_widget::MainWidget` while
/// rendering the upgraded `tg-ui`-styled profile cover, info rows, toggle switch,
/// and shared media tabs.
pub struct InfoWidget;

impl InfoWidget {
    /// Renders the peer info column or stacked section inside `ui`.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        ui: &mut Ui,
        peer: Option<&PeerSummary>,
        active_tab: &mut InfoTab,
        in_third_column: bool,
        palette: &Palette,
        _strings: &dyn Strings,
    ) -> Option<InfoAction> {
        let mode = if palette.window_bg.r() < 128 {
            ThemeMode::Night
        } else {
            ThemeMode::Day
        };
        let ui_colors = InfoColors::for_mode(mode);

        let Some(summary) = peer else {
            ui.painter()
                .rect_filled(ui.max_rect(), egui::CornerRadius::ZERO, palette.window_bg);
            ui.centered_and_justified(|ui| {
                ui.label(
                    egui::RichText::new(strings::PEER_UNAVAILABLE)
                        .font(tg_ui::fonts::font_id_from_style(
                            tg_ui::tg_style::st::normal_font,
                        ))
                        .color(palette.window_sub_text_fg),
                );
            });
            return None;
        };

        let mut backend = MockBackend::new();
        let target_id = if backend.peer_profile(summary.id).is_some() {
            summary.id
        } else {
            // Synthesize a temporary adapter view for any unknown shell PeerSummary.
            let profile = peer_profile_from_summary(summary);
            let mut single = SinglePeerFallback {
                profile,
                inner: backend,
            };
            let mut wrap = InfoWrapWidget::for_peer(summary.id);
            wrap.set_media_tab(info_tab_to_shared(active_tab));
            let events = wrap.show(ui, &mut single, in_third_column, &ui_colors);
            sync_info_tab(wrap.current_section(), active_tab);
            return map_wrap_events(&events, summary.id);
        };

        let mut wrap = InfoWrapWidget::for_peer(target_id);
        wrap.set_media_tab(info_tab_to_shared(active_tab));
        let events = wrap.show(ui, &mut backend, in_third_column, &ui_colors);
        sync_info_tab(wrap.current_section(), active_tab);
        map_wrap_events(&events, summary.id)
    }
}

fn info_tab_to_shared(tab: &InfoTab) -> SharedMediaTab {
    match tab {
        InfoTab::Overview | InfoTab::Media | InfoTab::Downloads => SharedMediaTab::PhotosVideos,
        InfoTab::Files => SharedMediaTab::Files,
        InfoTab::Links => SharedMediaTab::Links,
    }
}

fn sync_info_tab(section: InfoSection, active_tab: &mut InfoTab) {
    let media_tab = match section {
        InfoSection::Profile { media_tab, .. }
        | InfoSection::SharedMedia { tab: media_tab, .. } => media_tab,
        _ => return,
    };
    *active_tab = match media_tab {
        SharedMediaTab::PhotosVideos => InfoTab::Media,
        SharedMediaTab::Files => InfoTab::Files,
        SharedMediaTab::Links => InfoTab::Links,
        _ => InfoTab::Overview,
    };
}

fn map_wrap_events(events: &[InfoWrapEvent], peer_id: PeerId) -> Option<InfoAction> {
    for ev in events {
        match ev {
            InfoWrapEvent::CloseOrBack => return Some(InfoAction::CloseOrBack),
            InfoWrapEvent::OpenHistory(pid) => return Some(InfoAction::OpenHistory(*pid)),
            InfoWrapEvent::Toast(msg) if msg.starts_with("Notifications ") => {
                return Some(InfoAction::ToggleMute(peer_id));
            }
            _ => {}
        }
    }
    None
}

fn peer_profile_from_summary(summary: &PeerSummary) -> PeerProfile {
    let kind = if summary.is_saved_messages {
        InfoPeerKind::SavedMessages
    } else {
        match summary.kind {
            PeerKind::User => InfoPeerKind::User,
            PeerKind::Group => InfoPeerKind::Supergroup,
            PeerKind::Channel => InfoPeerKind::Channel,
        }
    };
    let usernames = summary
        .username
        .iter()
        .map(|u| PeerUsername {
            username: u.clone(),
            is_editable: true,
            is_active: true,
        })
        .collect();
    PeerProfile {
        id: summary.id,
        kind,
        title: summary.title.clone(),
        status: summary.status.clone(),
        is_online: summary.status == "online",
        verified: false,
        premium: false,
        color_index: (summary.id.bare() % 7) as u8,
        phone: (kind == InfoPeerKind::User)
            .then(|| summary.phone_or_link.clone())
            .flatten(),
        usernames,
        bio: summary.about.clone(),
        birthday: None,
        invite_link: (kind != InfoPeerKind::User)
            .then(|| summary.phone_or_link.clone())
            .flatten(),
        muted: summary.muted,
        is_contact: kind == InfoPeerKind::User,
        is_blocked: false,
        can_edit: false,
        can_add_members: false,
        can_manage_admins: false,
        can_manage_permissions: false,
        is_public: summary.username.is_some(),
        no_forwards: false,
        is_forum: false,
        members_count: 0,
        online_count: 0,
        admins_count: 0,
        default_banned_rights: ChatBannedRights::empty(),
        slowmode_seconds: 0,
        auto_delete_ttl_seconds: 0,
        linked_discussion_peer: None,
        bot_commands_count: 0,
        bot_reads_all_messages: false,
        has_mini_app: false,
        media_counts: SharedMediaCounts::default(),
        group_call: Default::default(),
    }
}

struct SinglePeerFallback {
    profile: PeerProfile,
    inner: MockBackend,
}

impl InfoBackend for SinglePeerFallback {
    fn peer_profile(&self, peer_id: PeerId) -> Option<PeerProfile> {
        if peer_id == self.profile.id {
            Some(self.profile.clone())
        } else {
            self.inner.peer_profile(peer_id)
        }
    }
    fn all_peers(&self) -> Vec<PeerProfile> {
        self.inner.all_peers()
    }
    fn contacts(&self) -> Vec<PeerProfile> {
        self.inner.contacts()
    }
    fn peer_members(&self, peer_id: PeerId, query: &str) -> Vec<MemberEntry> {
        self.inner.peer_members(peer_id, query)
    }
    fn common_groups(&self, user_id: PeerId) -> Vec<CommonGroupEntry> {
        self.inner.common_groups(user_id)
    }
    fn shared_media(&self, peer_id: PeerId, tab: SharedMediaTab, query: &str) -> Vec<MediaItem> {
        self.inner.shared_media(peer_id, tab, query)
    }
    fn toggle_notifications(&mut self, peer_id: PeerId) -> bool {
        if peer_id == self.profile.id {
            self.profile.muted = !self.profile.muted;
            self.profile.muted
        } else {
            self.inner.toggle_notifications(peer_id)
        }
    }
    fn toggle_block_user(&mut self, peer_id: PeerId) -> bool {
        self.inner.toggle_block_user(peer_id)
    }
    fn add_contact(&mut self, f: &str, l: &str, p: &str) -> Result<PeerId, String> {
        self.inner.add_contact(f, l, p)
    }
    fn create_group(&mut self, req: CreateGroupRequest) -> Result<PeerId, String> {
        self.inner.create_group(req)
    }
    fn create_channel(&mut self, req: CreateChannelRequest) -> Result<PeerId, String> {
        self.inner.create_channel(req)
    }
    fn update_peer_info(&mut self, id: PeerId, req: UpdatePeerInfoRequest) -> Result<(), String> {
        self.inner.update_peer_info(id, req)
    }
    fn update_default_permissions(
        &mut self,
        id: PeerId,
        rights: ChatBannedRights,
        slowmode: u32,
    ) -> Result<(), String> {
        self.inner.update_default_permissions(id, rights, slowmode)
    }
    fn update_admin_rights(
        &mut self,
        id: PeerId,
        uid: PeerId,
        rights: tg_data::rights::ChatAdminRights,
        title: String,
    ) -> Result<(), String> {
        self.inner.update_admin_rights(id, uid, rights, title)
    }
    fn add_members(&mut self, id: PeerId, member_ids: &[PeerId]) -> Result<usize, String> {
        self.inner.add_members(id, member_ids)
    }
    fn remove_member(&mut self, id: PeerId, uid: PeerId) -> Result<(), String> {
        self.inner.remove_member(id, uid)
    }
    fn set_auto_delete_ttl(&mut self, id: PeerId, ttl: u32) -> Result<(), String> {
        self.inner.set_auto_delete_ttl(id, ttl)
    }
    fn set_linked_discussion(
        &mut self,
        channel_id: PeerId,
        group_id: Option<PeerId>,
    ) -> Result<(), String> {
        self.inner.set_linked_discussion(channel_id, group_id)
    }
    fn check_username(&self, u: &str, exclude: Option<PeerId>) -> UsernameAvailability {
        self.inner.check_username(u, exclude)
    }
    fn update_username(&mut self, id: PeerId, u: &str) -> Result<(), String> {
        self.inner.update_username(id, u)
    }
    fn forward_messages(&mut self, t: &[PeerId], m: &[i32], c: &str) -> Result<usize, String> {
        self.inner.forward_messages(t, m, c)
    }
    fn delete_messages(&mut self, id: PeerId, m: &[i32], rev: bool) -> Result<usize, String> {
        self.inner.delete_messages(id, m, rev)
    }
    fn submit_report(&mut self, id: PeerId, r: ReportReason, c: &str) -> Result<(), String> {
        self.inner.submit_report(id, r, c)
    }
    fn languages(&self) -> Vec<LanguageEntry> {
        self.inner.languages()
    }
    fn active_language(&self) -> String {
        self.inner.active_language()
    }
    fn set_language(&mut self, code: &str) -> Result<(), String> {
        self.inner.set_language(code)
    }
    fn terms_of_service(&self) -> TermsOfServiceData {
        self.inner.terms_of_service()
    }
    fn accept_terms(&mut self, tos_id: &str) -> Result<(), String> {
        self.inner.accept_terms(tos_id)
    }
    fn exported_chat_invites(
        &self,
        peer_id: PeerId,
        revoked: bool,
    ) -> Result<InviteLinksList, String> {
        self.inner.exported_chat_invites(peer_id, revoked)
    }
    fn export_chat_invite(
        &mut self,
        peer_id: PeerId,
        req: InviteLinkRequest,
    ) -> Result<InviteLinkInfo, String> {
        self.inner.export_chat_invite(peer_id, req)
    }
    fn edit_exported_chat_invite(
        &mut self,
        peer_id: PeerId,
        link: &str,
        req: InviteLinkRequest,
    ) -> Result<InviteLinkInfo, String> {
        self.inner.edit_exported_chat_invite(peer_id, link, req)
    }
    fn revoke_exported_chat_invite(
        &mut self,
        peer_id: PeerId,
        link: &str,
    ) -> Result<(InviteLinkInfo, Option<InviteLinkInfo>), String> {
        self.inner.revoke_exported_chat_invite(peer_id, link)
    }
    fn delete_exported_chat_invite(&mut self, peer_id: PeerId, link: &str) -> Result<(), String> {
        self.inner.delete_exported_chat_invite(peer_id, link)
    }
    fn delete_revoked_exported_chat_invites(&mut self, peer_id: PeerId) -> Result<(), String> {
        self.inner.delete_revoked_exported_chat_invites(peer_id)
    }
    fn chat_invite_importers(
        &self,
        peer_id: PeerId,
        link: Option<&str>,
        requested: bool,
        query: &str,
    ) -> Result<InviteImportersList, String> {
        self.inner
            .chat_invite_importers(peer_id, link, requested, query)
    }
    fn hide_chat_join_request(
        &mut self,
        peer_id: PeerId,
        user_id: PeerId,
        approved: bool,
    ) -> Result<(), String> {
        self.inner
            .hide_chat_join_request(peer_id, user_id, approved)
    }
    fn hide_all_chat_join_requests(
        &mut self,
        peer_id: PeerId,
        link: Option<&str>,
        approved: bool,
    ) -> Result<usize, String> {
        self.inner
            .hide_all_chat_join_requests(peer_id, link, approved)
    }
    fn join_requests_summary(&self, peer_id: PeerId) -> JoinRequestsSummary {
        self.inner.join_requests_summary(peer_id)
    }
    fn peer_color_settings(&self, peer_id: PeerId) -> PeerColorSettings {
        self.inner.peer_color_settings(peer_id)
    }
    fn update_peer_color(
        &mut self,
        peer_id: PeerId,
        req: UpdatePeerColorRequest,
    ) -> Result<(), String> {
        self.inner.update_peer_color(peer_id, req)
    }
    fn check_ownership_transfer(&self, peer_id: PeerId) -> Result<OwnershipTransferCheck, String> {
        self.inner.check_ownership_transfer(peer_id)
    }
    fn transfer_ownership(
        &mut self,
        peer_id: PeerId,
        new_owner_id: PeerId,
        password: &str,
    ) -> Result<(), OwnershipTransferError> {
        self.inner
            .transfer_ownership(peer_id, new_owner_id, password)
    }
    fn similar_channels(&self, peer_id: PeerId) -> Result<SimilarChannelsResult, String> {
        self.inner.similar_channels(peer_id)
    }
    fn downloads_list(&self, query: &str) -> Vec<DownloadEntry> {
        self.inner.downloads_list(query)
    }
    fn toggle_download_pause(&mut self, download_id: u64) -> Result<(), String> {
        self.inner.toggle_download_pause(download_id)
    }
    fn cancel_or_delete_download(
        &mut self,
        download_id: u64,
        delete_file: bool,
    ) -> Result<(), String> {
        self.inner
            .cancel_or_delete_download(download_id, delete_file)
    }
    fn clear_completed_downloads(&mut self) -> Result<usize, String> {
        self.inner.clear_completed_downloads()
    }
    fn delete_all_downloads(&mut self) -> Result<usize, String> {
        self.inner.delete_all_downloads()
    }
    fn global_media(&self, tab: SharedMediaTab, query: &str, limit: usize) -> GlobalMediaPage {
        self.inner.global_media(tab, query, limit)
    }
    fn peer_pinned_stories(
        &self,
        peer_id: PeerId,
        offset_id: i32,
        limit: i32,
    ) -> Result<ProfileStoriesPage, String> {
        self.inner.peer_pinned_stories(peer_id, offset_id, limit)
    }
    fn peer_stories_archive(
        &self,
        peer_id: PeerId,
        offset_id: i32,
        limit: i32,
    ) -> Result<ProfileStoriesPage, String> {
        self.inner.peer_stories_archive(peer_id, offset_id, limit)
    }
    fn peer_story_albums(&self, peer_id: PeerId) -> Result<Vec<ProfileStoryAlbum>, String> {
        self.inner.peer_story_albums(peer_id)
    }
    fn peer_album_stories(
        &self,
        peer_id: PeerId,
        album_id: i32,
        offset_id: i32,
        limit: i32,
    ) -> Result<ProfileStoriesPage, String> {
        self.inner
            .peer_album_stories(peer_id, album_id, offset_id, limit)
    }
    fn peer_saved_gifts(
        &self,
        peer_id: PeerId,
        filter: &tg_payments::star_gifts::PeerGiftsFilter,
    ) -> Result<ProfilePeerGiftsPage, String> {
        self.inner.peer_saved_gifts(peer_id, filter)
    }
    fn toggle_peer_gift_saved(
        &mut self,
        peer_id: PeerId,
        msg_id: i32,
        saved_id: u64,
        unsave: bool,
    ) -> Result<(), String> {
        self.inner
            .toggle_peer_gift_saved(peer_id, msg_id, saved_id, unsave)
    }
    fn peer_saved_music(
        &self,
        peer_id: PeerId,
        offset: i32,
        limit: i32,
    ) -> Result<Vec<SavedMusicTrack>, String> {
        self.inner.peer_saved_music(peer_id, offset, limit)
    }
    fn save_profile_music(&mut self, track: &SavedMusicTrack, unsave: bool) -> Result<(), String> {
        self.inner.save_profile_music(track, unsave)
    }
    fn upload_emoji_userpic(
        &mut self,
        peer_id: PeerId,
        req: EmojiUserpicRequest,
    ) -> Result<(), String> {
        self.inner.upload_emoji_userpic(peer_id, req)
    }
    fn peer_available_reactions(&self, peer_id: PeerId) -> PeerReactionsConfig {
        self.inner.peer_available_reactions(peer_id)
    }
    fn set_peer_available_reactions(
        &mut self,
        peer_id: PeerId,
        config: PeerReactionsConfig,
    ) -> Result<(), String> {
        self.inner.set_peer_available_reactions(peer_id, config)
    }
    fn toggle_peer_username(
        &mut self,
        peer_id: PeerId,
        username: &str,
        active: bool,
    ) -> Result<(), String> {
        self.inner.toggle_peer_username(peer_id, username, active)
    }
    fn reorder_peer_usernames(&mut self, peer_id: PeerId, order: &[String]) -> Result<(), String> {
        self.inner.reorder_peer_usernames(peer_id, order)
    }
    fn peer_verification_status(&self, bot_id: PeerId, peer_id: PeerId) -> PeerVerificationStatus {
        self.inner.peer_verification_status(bot_id, peer_id)
    }
    fn set_peer_custom_verification(
        &mut self,
        bot_id: PeerId,
        peer_id: PeerId,
        enabled: bool,
        custom_description: Option<&str>,
    ) -> Result<(), String> {
        self.inner
            .set_peer_custom_verification(bot_id, peer_id, enabled, custom_description)
    }
    fn get_collectible_info(
        &self,
        peer_id: PeerId,
        entity: &str,
    ) -> Result<crate::boxes::collectible_info::CollectibleInfoData, String> {
        self.inner.get_collectible_info(peer_id, entity)
    }
    fn set_main_profile_tab(&mut self, peer_id: PeerId, tab: ProfileMainTab) -> Result<(), String> {
        self.inner.set_main_profile_tab(peer_id, tab)
    }
    fn main_profile_tab(&self, peer_id: PeerId) -> Option<ProfileMainTab> {
        self.inner.main_profile_tab(peer_id)
    }
    fn suggest_user_birthday(&mut self, peer_id: PeerId, birthday: Birthday) -> Result<(), String> {
        self.inner.suggest_user_birthday(peer_id, birthday)
    }
    fn report_profile_photo(
        &mut self,
        peer_id: PeerId,
        photo: ProfilePhotoRef,
        reason: ReportReason,
        comment: &str,
    ) -> Result<(), String> {
        self.inner
            .report_profile_photo(peer_id, photo, reason, comment)
    }
    fn personal_channel(&self, peer_id: PeerId) -> Option<PersonalChannelOption> {
        self.inner.personal_channel(peer_id)
    }
    fn personal_channel_candidates(&self) -> Vec<PersonalChannelOption> {
        self.inner.personal_channel_candidates()
    }
    fn update_personal_channel(&mut self, channel_id: Option<PeerId>) -> Result<(), String> {
        self.inner.update_personal_channel(channel_id)
    }
    fn peer_profile_balance(&self, peer_id: PeerId) -> Option<ProfileBalanceInfo> {
        self.inner.peer_profile_balance(peer_id)
    }
    fn chat_star_gift_notifications_enabled(&self, peer_id: PeerId) -> Option<bool> {
        self.inner.chat_star_gift_notifications_enabled(peer_id)
    }
    fn toggle_chat_star_gift_notifications(
        &mut self,
        peer_id: PeerId,
        enabled: bool,
    ) -> Result<bool, String> {
        self.inner
            .toggle_chat_star_gift_notifications(peer_id, enabled)
    }
    fn channel_management_state(
        &self,
        peer_id: PeerId,
    ) -> Option<crate::info::backend::ChannelManagementState> {
        self.inner.channel_management_state(peer_id)
    }
    fn toggle_pre_history_hidden(&mut self, peer_id: PeerId, hidden: bool) -> Result<(), String> {
        self.inner.toggle_pre_history_hidden(peer_id, hidden)
    }
    fn groups_for_discussion(
        &self,
    ) -> Result<Vec<crate::info::backend::DiscussionGroupCandidate>, String> {
        self.inner.groups_for_discussion()
    }
    fn convert_to_gigagroup(&mut self, peer_id: PeerId) -> Result<(), String> {
        self.inner.convert_to_gigagroup(peer_id)
    }
    fn deactivate_all_channel_usernames(&mut self, peer_id: PeerId) -> Result<(), String> {
        self.inner.deactivate_all_channel_usernames(peer_id)
    }
    fn toggle_channel_forum(
        &mut self,
        peer_id: PeerId,
        enabled: bool,
        tabs: bool,
    ) -> Result<(), String> {
        self.inner.toggle_channel_forum(peer_id, enabled, tabs)
    }
    fn toggle_participants_hidden(&mut self, peer_id: PeerId, hidden: bool) -> Result<(), String> {
        self.inner.toggle_participants_hidden(peer_id, hidden)
    }
    fn toggle_view_forum_as_messages(
        &mut self,
        peer_id: PeerId,
        as_messages: bool,
    ) -> Result<(), String> {
        self.inner
            .toggle_view_forum_as_messages(peer_id, as_messages)
    }
    fn set_boosts_to_unblock_restrictions(
        &mut self,
        peer_id: PeerId,
        boosts: u32,
    ) -> Result<(), String> {
        self.inner
            .set_boosts_to_unblock_restrictions(peer_id, boosts)
    }
    fn set_channel_sticker_set(
        &mut self,
        peer_id: PeerId,
        set: crate::info::backend::ChannelStickerSetRef,
    ) -> Result<(), String> {
        self.inner.set_channel_sticker_set(peer_id, set)
    }
    fn set_channel_emoji_sticker_set(
        &mut self,
        peer_id: PeerId,
        set: crate::info::backend::ChannelStickerSetRef,
    ) -> Result<(), String> {
        self.inner.set_channel_emoji_sticker_set(peer_id, set)
    }
    fn update_channel_paid_messages_price(
        &mut self,
        peer_id: PeerId,
        broadcast_messages_allowed: bool,
        stars_per_message: i64,
    ) -> Result<(), String> {
        self.inner.update_channel_paid_messages_price(
            peer_id,
            broadcast_messages_allowed,
            stars_per_message,
        )
    }
    fn toggle_channel_autotranslation(
        &mut self,
        peer_id: PeerId,
        enabled: bool,
    ) -> Result<(), String> {
        self.inner.toggle_channel_autotranslation(peer_id, enabled)
    }
    fn delete_basic_chat(&mut self, peer_id: PeerId) -> Result<(), String> {
        self.inner.delete_basic_chat(peer_id)
    }
    fn admins_with_invites(
        &self,
        peer_id: PeerId,
    ) -> Result<Vec<crate::info::backend::AdminWithInvitesEntry>, String> {
        self.inner.admins_with_invites(peer_id)
    }
    fn delete_contacts(&mut self, peer_ids: &[PeerId]) -> Result<(), String> {
        self.inner.delete_contacts(peer_ids)
    }
    fn export_contact_token(
        &self,
    ) -> Result<tg_app_core::core_types::dto::contacts_extras::ExportedContactTokenInfo, String>
    {
        self.inner.export_contact_token()
    }
    fn import_contact_token(&mut self, token: &str) -> Result<PeerId, String> {
        self.inner.import_contact_token(token)
    }
    fn export_no_paid_messages_token(
        &self,
        revoke: bool,
        temporary: bool,
    ) -> Result<
        tg_app_core::core_types::dto::contacts_extras::ExportedNoPaidMessagesTokenInfo,
        String,
    > {
        self.inner.export_no_paid_messages_token(revoke, temporary)
    }
    fn import_no_paid_messages_token(
        &mut self,
        peer_id: PeerId,
        token: &str,
    ) -> Result<
        tg_app_core::core_types::dto::contacts_extras::NoPaidMessagesTokenImportResult,
        String,
    > {
        self.inner.import_no_paid_messages_token(peer_id, token)
    }
    fn contact_note(&self, peer_id: PeerId) -> Option<tg_text::TextWithEntities> {
        self.inner.contact_note(peer_id)
    }
    fn update_contact_note(
        &mut self,
        peer_id: PeerId,
        note: tg_text::TextWithEntities,
    ) -> Result<bool, String> {
        self.inner.update_contact_note(peer_id, note)
    }
    fn delete_profile_photos(
        &mut self,
        photos: &[crate::info::backend::ProfilePhotoRef],
    ) -> Result<Vec<i64>, String> {
        self.inner.delete_profile_photos(photos)
    }
    fn upload_contact_profile_photo(
        &mut self,
        peer_id: PeerId,
        request: tg_app_core::core_types::dto::contacts_extras::ContactProfilePhotoUpload,
    ) -> Result<i64, String> {
        self.inner.upload_contact_profile_photo(peer_id, request)
    }
}
